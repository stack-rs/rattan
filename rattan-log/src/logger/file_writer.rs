use tokio::sync::mpsc::UnboundedReceiver;
use tracing::info;

use super::mmap::*;
use super::LOGGING_TX;
use super::{LOG_ENTRY_CHUNK_SIZE, META_DATA_CHUNK_SIZE};
use crate::blob::RelativePointer;
use crate::log_entry::entry::{
    chunk_header::{new_log_entry_chunk_prologue, new_meta_chunk_prologue, ChunkPrologue},
    flow_entry::FlowEntryVariant,
};

use crate::PlainBytes;
use crate::RattanLogOp;
use crate::RawLogEntry;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};

use std::io::{BufWriter, Result, Write};

use std::path::PathBuf;

fn new_file(path: &PathBuf) -> File {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(true)
        .open(path)
        .unwrap()
}

struct BlobWriter {
    written: u64,
    writer: BufWriter<File>,
}

impl BlobWriter {
    fn new(path: &PathBuf) -> Self {
        Self {
            written: 0,
            writer: BufWriter::new(new_file(path)),
        }
    }
    fn write(&mut self, data: &[u8]) -> Result<(u64, usize)> {
        let start = self.written;
        let len = data.len();
        self.writer.write_all(data)?;
        self.written += len as u64;
        Ok((start, len))
    }
}

impl Drop for BlobWriter {
    fn drop(&mut self) {
        info!("Blob Writer wrote total {} Bytes", self.written);
        self.writer.flush().unwrap();
    }
}

struct EntryChunkWriter {
    flow_entries: Vec<Vec<u8>>,
    entry_file: File,
    ref_offset: Option<u64>,
    current_chunk_offset: u64,
    buffer: Vec<u8>,
}

impl EntryChunkWriter {
    fn new(path: &PathBuf) -> Result<Self> {
        let entry_file = new_file(path);
        let mut buffer = Vec::with_capacity(LOG_ENTRY_CHUNK_SIZE);
        buffer.resize(size_of::<ChunkPrologue>(), 0);

        Ok(Self {
            flow_entries: vec![],
            entry_file,
            ref_offset: None,
            current_chunk_offset: META_DATA_CHUNK_SIZE,
            buffer,
        })
    }

    fn expand_log_entry_file(&self, target: u64) -> Result<()> {
        if target >= self.entry_file.metadata()?.len() {
            let extra = (16 * LOG_ENTRY_CHUNK_SIZE) as u64;
            self.entry_file.set_len(target + extra)?;
            tracing::debug!(
                "Expanding log_entry_file to logical size {}",
                target + extra
            );
        }
        Ok(())
    }

    fn update_entry_chunk_prologue(&mut self, chunk_size: usize) {
        if self.buffer.is_empty() {
            return;
        }
        const CHUNK_PROLOGUE_SIZE: usize = size_of::<ChunkPrologue>();
        debug_assert!(self.buffer.len() <= chunk_size);
        debug_assert!(self.buffer.len() >= CHUNK_PROLOGUE_SIZE);

        assert_eq!(chunk_size % PAGE_SIZE as usize, 0);

        let log_chunk_prologue = new_log_entry_chunk_prologue(
            self.ref_offset.take().unwrap_or_default(),
            (chunk_size / PAGE_SIZE as usize) as u32, // should be 256 for 1MiB chunk
            self.buffer.len(),
        );
        self.buffer[0..CHUNK_PROLOGUE_SIZE].copy_from_slice(log_chunk_prologue.as_bytes());
    }

    fn write_log_chunk(&mut self, chunk_size: usize, write_at: u64) -> Result<bool> {
        if self.buffer.is_empty() {
            return Ok(false);
        }

        tracing::debug!(
            "Try to mmap at [{},{}) in log_entry_flie",
            write_at,
            chunk_size as u64 + write_at
        );

        self.expand_log_entry_file(write_at + chunk_size as u64)?;

        let mut mmap = mmap_mut_segment(&self.entry_file, write_at, chunk_size)?;

        mmap[0..self.buffer.len()].copy_from_slice(&self.buffer);
        self.buffer.clear();

        Ok(true)
    }

    fn prepare_buffer(&mut self, length_to_add: usize) -> Result<()> {
        if self.buffer.len() + length_to_add > LOG_ENTRY_CHUNK_SIZE {
            self.update_entry_chunk_prologue(LOG_ENTRY_CHUNK_SIZE);
            self.write_log_chunk(LOG_ENTRY_CHUNK_SIZE, self.current_chunk_offset)?;
            self.current_chunk_offset += LOG_ENTRY_CHUNK_SIZE as u64;
            self.buffer.resize(size_of::<ChunkPrologue>(), 0);
        };

        // This falls only when the chunk is too small to contain a single log entry.
        debug_assert!(self.buffer.len() + length_to_add <= LOG_ENTRY_CHUNK_SIZE);
        Ok(())
    }

    fn add_log_entry(&mut self, entry: &[u8]) -> Result<()> {
        self.prepare_buffer(entry.len())?;
        self.buffer.extend_from_slice(entry);
        debug_assert!(self.buffer.len() <= LOG_ENTRY_CHUNK_SIZE);
        Ok(())
    }

    fn add_raw_log_entry(
        &mut self,
        mut entry: RawLogEntry,
        offset: u64,
        size: usize,
    ) -> Result<()> {
        self.prepare_buffer(size_of_val(&entry))?;

        if self.ref_offset.is_none() {
            self.ref_offset = Some(offset);
        }
        let relative_offset = (offset - self.ref_offset.unwrap()) as u32;
        assert!(relative_offset <= 1 << 24);
        assert!(size <= 256);

        let mut blob_pointer = RelativePointer::new();
        blob_pointer.set_length(size as u8);
        blob_pointer.set_offset(relative_offset);
        entry.raw_entry.pointer = blob_pointer;

        self.add_log_entry(entry.as_bytes())
    }

    fn add_flow_entry(&mut self, flow_entry: Vec<u8>) {
        self.flow_entries.push(flow_entry);
    }

    fn finalize(mut self) -> Result<u64> {
        let file_length = self.finalize_inner()?;
        self.entry_file.set_len(file_length)?;
        tracing::debug!("Entry file length {}Bytes", file_length);
        Ok(file_length)
    }

    // Set the file_length to this number afther this function call.
    fn finalize_inner(&mut self) -> Result<u64> {
        self.update_entry_chunk_prologue(LOG_ENTRY_CHUNK_SIZE);
        if self.write_log_chunk(LOG_ENTRY_CHUNK_SIZE, self.current_chunk_offset)? {
            self.current_chunk_offset += LOG_ENTRY_CHUNK_SIZE as u64;
        }
        let mut file_length = self.current_chunk_offset;

        self.buffer.clear();

        if self.flow_entries.is_empty() {
            tracing::warn!("NO flow entries");
            return Ok(file_length);
        } else {
            tracing::info!("{} flow entries to be written...", self.flow_entries.len());
        }
        let flow_entries = std::mem::take(&mut self.flow_entries);

        let mut flow_entries = flow_entries.into_iter().peekable();
        self.buffer = Vec::with_capacity(META_DATA_CHUNK_SIZE as usize);

        let mut page_offset = 0;
        loop {
            self.buffer.clear();
            self.buffer.resize(size_of::<ChunkPrologue>(), 0);

            while flow_entries
                .peek()
                .is_some_and(|next| next.len() + self.buffer.len() <= META_DATA_CHUNK_SIZE as usize)
            {
                self.buffer
                    .extend_from_slice(flow_entries.next().unwrap().as_slice());
            }

            let next_page_offset = flow_entries
                .peek()
                .is_some()
                .then_some(self.current_chunk_offset);

            let meta_data_epilogue = new_meta_chunk_prologue(
                next_page_offset.unwrap_or_default(),
                (META_DATA_CHUNK_SIZE / PAGE_SIZE) as u32,
                self.buffer.len(),
            );
            self.buffer[0..size_of::<ChunkPrologue>()]
                .copy_from_slice(meta_data_epilogue.as_bytes());

            self.current_chunk_offset += META_DATA_CHUNK_SIZE;

            self.write_log_chunk(META_DATA_CHUNK_SIZE as usize, page_offset)?;

            file_length = file_length.max(page_offset + META_DATA_CHUNK_SIZE);

            if let Some(next) = next_page_offset {
                page_offset = next;
            } else {
                return Ok(file_length);
            }
        }
    }
}

fn writting(
    mut path: PathBuf,
    mut log_rx: UnboundedReceiver<RattanLogOp>,
    raw: bool,
) -> Result<()> {
    let _span = tracing::span!(tracing::Level::DEBUG, "writting thread").entered();

    tracing::info!("writing thread started");

    let mut entry_writer = EntryChunkWriter::new(&path)?;
    path.set_extension("raw");
    let mut raw_blob = raw.then(|| BlobWriter::new(&path));
    let mut flow_cnt: u16 = 0;

    let mut flows = HashMap::new();
    while let Some(entry) = log_rx.blocking_recv() {
        match entry {
            RattanLogOp::Entry(entry) => entry_writer.add_log_entry(entry.as_slice())?,

            RattanLogOp::Flow(flow_id, base_ts, flow_desc) => {
                flows.entry(flow_id).or_insert_with(|| {
                    // flow_index starts from 1
                    flow_cnt += 1;
                    flow_cnt
                });
                let entry = FlowEntryVariant::from((flow_id, base_ts, flow_desc, flow_cnt)).build();
                entry_writer.add_flow_entry(entry);
            }
            RattanLogOp::RawEntry(flow_id, mut entry, raw) => {
                // 0 for unknown flow_id
                let flow_index = flows.get(&flow_id).cloned().unwrap_or_default();
                entry.raw_entry.flow_index = flow_index;
                let (offset, size) = raw_blob.as_mut().unwrap().write(&raw)?;
                entry_writer.add_raw_log_entry(entry, offset, size)?;
            }
            RattanLogOp::End => {
                tracing::debug!("Logging thread exit");
                drop(raw_blob);
                entry_writer.finalize()?;
                break;
            }
        }
    }

    Ok(())
}

pub fn file_logging_thread(log_path: PathBuf) -> std::thread::JoinHandle<Result<()>> {
    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
    LOGGING_TX.set(log_tx).unwrap();
    std::thread::spawn(move || writting(log_path, log_rx, false))
}

pub fn file_logging_thread_raw(log_path: PathBuf) -> std::thread::JoinHandle<Result<()>> {
    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
    LOGGING_TX.set(log_tx).unwrap();
    std::thread::spawn(move || writting(log_path, log_rx, true))
}
