use std::collections::HashMap;
use std::io::Error;
use std::io::Result;
use std::path::PathBuf;

use tokio::sync::mpsc::UnboundedReceiver;

use super::mmap::*;
use super::LOGGING_TX;
use crate::blob::RelativePointer;
use crate::log_entry::entry::chunk_header::ChunkPrologue;
use crate::log_entry::entry::{
    chunk_header::new_log_entry_chunk_prologue, flow_entry::FlowEntryVariant,
};
use crate::PlainBytes;
use crate::RattanLogOp;
use crate::RawLogEntry;

const MMAP_CHUNK_SIZE_16M: usize = 4096;
const MMAP_CHUNK_SIZE_4K: usize = 1;
const LOGICAL_CHUNK_SIZE_1M: u32 = 256;

struct EntryWriter<H, C>
where
    H: FnMut(usize, &Option<C>) -> Vec<u8>,
{
    base_path: PathBuf,
    log_entry_file: Option<MmapChunkWriter<MMAP_CHUNK_SIZE_16M, H, C>>,
    log_ref_offset: Option<u64>,
    old_log_entry_file: Option<MmapStreamWriter<MMAP_CHUNK_SIZE_16M>>,
    raw_file: Option<MmapStreamWriter<MMAP_CHUNK_SIZE_16M>>,
    flow_file: Option<MmapStreamWriter<MMAP_CHUNK_SIZE_4K>>,
}

impl<H> EntryWriter<H, u64>
where
    H: FnMut(usize, &Option<u64>) -> Vec<u8>,
{
    fn new(base_path: PathBuf) -> Self {
        Self {
            base_path,
            log_entry_file: None,
            old_log_entry_file: None,
            raw_file: None,
            flow_file: None,
            log_ref_offset: None,
        }
    }

    fn add_raw_log_entry(
        &mut self,
        mut entry: RawLogEntry,
        offset: u64,
        length: usize,
        func: H,
    ) -> Result<()> {
        let raw_log_file = match self.log_entry_file {
            Some(ref mut f) => f,
            None => {
                self.base_path.set_extension("rtl");
                let file = MmapChunkWriter::new_truncate(
                    &self.base_path,
                    func,
                    size_of::<ChunkPrologue>(),
                    LOGICAL_CHUNK_SIZE_1M as usize * PAGE_SIZE as usize,
                )?;
                self.log_entry_file.insert(file)
            }
        };

        let (is_first, allocation) = raw_log_file.allocate(size_of_val(&entry))?;

        if is_first {
            self.log_ref_offset = Some(offset);
        }
        let log_ref_offset = self.log_ref_offset.unwrap_or_default();

        let mut blob_pointer = RelativePointer::new();
        blob_pointer.set_length(length as u8);
        let relative_offset = offset - log_ref_offset;
        blob_pointer.set_offset(relative_offset as u32);

        if length > 256 {
            return Err(Error::new(
                std::io::ErrorKind::Unsupported,
                "Raw headers shall not longer than 256B ",
            ));
        }

        if relative_offset > (1 << 24) {
            // This shall never be exceed in the following setting:
            // - Each chunk is 1048576 (1 << 20) bytes in size,
            // - Each raw log entry is 16 (1 << 4) bytes in size,
            // - Each raw header is no longer than 256 (1 << 8) bytes in size.
            // - Raw headers are wirtten continuously.
            //
            // The max relative offset ever possible is ((1 << 20) / (1 << 4) - 1) * (1 << 8),
            // which is 16,776,960 B, smaller than (1 << 24)B, or 16,777,216 B.
            return Err(Error::new(
                std::io::ErrorKind::Unsupported,
                "All raw log entries in the same chunk should contain at most 16MiB bytes of raw headers. \
                If this is violated, try use smaller chunk size.
                ",
            ));
        }

        entry.raw_entry.pointer = blob_pointer;

        allocation.consume(entry.as_bytes(), is_first.then_some(offset));
        Ok(())
    }

    fn add_log_entry(&mut self, entry: &[u8]) -> Result<()> {
        match self.old_log_entry_file {
            Some(ref mut f) => f,
            None => {
                self.base_path.set_extension("rtl");
                let file = MmapStreamWriter::new_truncate(&self.base_path)?;
                self.old_log_entry_file.insert(file)
            }
        }
        .extend_from_slice(entry)?;
        Ok(())
    }

    fn add_flow_entry(&mut self, entry: &[u8]) -> Result<()> {
        match self.flow_file {
            Some(ref mut f) => f,
            None => {
                self.base_path.set_extension("flow");
                let file = MmapStreamWriter::new_truncate(&self.base_path)?;
                self.flow_file.insert(file)
            }
        }
        .extend_from_slice(entry)?;
        Ok(())
    }

    fn add_raw(&mut self, header: &[u8]) -> Result<(u64, usize)> {
        match self.raw_file {
            Some(ref mut f) => f,
            None => {
                self.base_path.set_extension("raw");
                let file = MmapStreamWriter::new_truncate(&self.base_path)?;
                self.raw_file.insert(file)
            }
        }
        .extend_from_slice(header)
    }
}

fn build_chunk_prologue(data_length: usize, time_offset: &Option<u64>) -> Vec<u8> {
    let header = new_log_entry_chunk_prologue(
        time_offset.unwrap_or_default(),
        LOGICAL_CHUNK_SIZE_1M,
        data_length,
    );
    header.as_bytes().to_owned()
}

fn writing(path: PathBuf, mut log_rx: UnboundedReceiver<RattanLogOp>) -> Result<()> {
    let _span = tracing::span!(tracing::Level::DEBUG, "writing thread").entered();

    tracing::debug!("Packet logging thread started");

    let mut entry_writer = EntryWriter::new(path);

    let mut flows = HashMap::new();
    while let Some(entry) = log_rx.blocking_recv() {
        match entry {
            RattanLogOp::Entry(entry) => entry_writer.add_log_entry(entry.as_slice())?,
            RattanLogOp::Flow(flow_id, base_ts, flow_desc) => {
                let flow_index = 1 + flows.len();
                flows.entry(flow_id).or_insert(flow_index as u16);
                let entry = FlowEntryVariant::from((flow_id, base_ts, flow_desc)).build();
                entry_writer.add_flow_entry(&entry)?;
            }
            RattanLogOp::RawEntry(flow_id, mut entry, raw) => {
                // 0 for unknown flow_id
                let flow_index = flows.get(&flow_id).cloned().unwrap_or_default();
                entry.raw_entry.flow_index = flow_index;
                let (offset, size) = entry_writer.add_raw(&raw)?;
                entry_writer.add_raw_log_entry(entry, offset, size, build_chunk_prologue)?;
            }
            RattanLogOp::End => {
                break;
            }
        }
    }

    tracing::debug!("Packet logging thread exited");
    Ok(())
}

pub fn file_logging_thread(log_path: PathBuf) -> std::thread::JoinHandle<Result<()>> {
    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
    LOGGING_TX
        .set(log_tx)
        .expect("LOGGING_TX should only be set here");
    std::thread::spawn(move || writing(log_path, log_rx))
}
