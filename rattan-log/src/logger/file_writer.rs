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
use std::collections::HashMap;

use std::io::Result;

use std::path::PathBuf;

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
                self.log_entry_file = Some(file);
                self.log_entry_file.as_mut().unwrap()
            }
        };

        let (is_first, allocation) = raw_log_file.allocate(size_of_val(&entry))?;

        if is_first {
            self.log_ref_offset = Some(offset);
        }
        let log_ref_offset = self.log_ref_offset.unwrap();

        let mut blob_pointer = RelativePointer::new();
        blob_pointer.set_length(length as u8);
        let relative_offset = offset - log_ref_offset;
        blob_pointer.set_offset(relative_offset as u32);
        assert!(length <= 256);
        assert!(relative_offset <= 1 << 24);

        entry.raw_entry.pointer = blob_pointer;

        allocation.consume(entry.as_bytes(), is_first.then_some(offset));
        Ok(())
    }

    fn add_log_entry(&mut self, entry: &[u8]) -> Result<()> {
        match self.old_log_entry_file {
            Some(ref mut f) => f,
            None => {
                self.base_path.set_extension("oldrtl");
                let file = MmapStreamWriter::new_truncate(&self.base_path)?;
                let _ = self.old_log_entry_file.insert(file);
                self.old_log_entry_file.as_mut().unwrap()
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
                let _ = self.flow_file.insert(file);
                self.flow_file.as_mut().unwrap()
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
                let _ = self.raw_file.insert(file);
                self.raw_file.as_mut().unwrap()
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

fn writting(path: PathBuf, mut log_rx: UnboundedReceiver<RattanLogOp>) -> Result<()> {
    let _span = tracing::span!(tracing::Level::DEBUG, "writting thread").entered();

    tracing::info!("writing thread started");

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
                tracing::debug!("Logging thread exit");
                break;
            }
        }
    }

    Ok(())
}

pub fn file_logging_thread(log_path: PathBuf) -> std::thread::JoinHandle<Result<()>> {
    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
    LOGGING_TX.set(log_tx).unwrap();
    std::thread::spawn(move || writting(log_path, log_rx))
}

pub fn file_logging_thread_raw(log_path: PathBuf) -> std::thread::JoinHandle<Result<()>> {
    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
    LOGGING_TX.set(log_tx).unwrap();
    std::thread::spawn(move || writting(log_path, log_rx))
}
