use super::LogEntryHeader;
use binread::BinRead;
use plain::Plain;

// The detailed spec of chunk prologue:
//
// It is 32Bytes, so that the 32Bytes Log Entries can be 32-byte aligned.
//
// 0                   1                   2                   3
// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |       LH.length       | LH.ty.|      CH.length        | CH.ty.|
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                     log    version                            |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                         data  length                          |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                        chunk  length                          |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                   offset (lower 32bits)                       |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                   offset (upper 32bits)                       |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                          reserved                             |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                          reserved                             |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

#[derive(Debug, Clone, Copy, BinRead, Default)]
#[br(import(header: LogEntryHeader))]
#[repr(C, packed(2))]
pub struct ChunkPrologue {
    #[br(calc = header)]
    pub header: LogEntryHeader,
    pub chunk: ChunkEntry,
}

unsafe impl Plain for ChunkPrologue {}

pub type ChunkHeader = LogEntryHeader;

#[derive(Debug, Clone, Copy, BinRead, Default)]
#[repr(C, packed(2))]
pub struct ChunkEntry {
    pub header: ChunkHeader,
    pub log_version: u32,
    // Length of all following data and the chunk prologue, excluding padding at the end of the chunk
    pub data_length: u32,
    // size of this chunk, in bytes
    pub chunk_size: u32,
    // All the Raw Log entry's offset is relative to this.
    pub offset: u64,
    pub _reserved: u64,
}

pub const TYPE_LOG_ENTRY: u8 = 0;
pub const TYPE_LOG_META: u8 = 1;

pub fn new_log_entry_chunk_prologue(
    ref_offset: u64,
    chunk_size_pages: u32,
    chunk_len: usize,
) -> ChunkPrologue {
    let mut prologue = ChunkPrologue::default();
    prologue.header.set_length(32);
    prologue.header.set_type(15);
    prologue.chunk.header.set_length(30);
    prologue.chunk.header.set_type(TYPE_LOG_ENTRY);
    prologue.chunk.offset = ref_offset;
    prologue.chunk.data_length = chunk_len as u32;
    prologue.chunk.log_version = 0x20251120;
    prologue.chunk.chunk_size = 4096 * chunk_size_pages;
    prologue
}

/// If next_offset = 0, this is the last meta_chunk
/// Otherwise, the next meta_chunk should be at next_offset from the start of the file.
pub fn new_meta_chunk_prologue(
    next_offset: u64,
    chunk_size_pages: u32,
    chunk_len: usize,
) -> ChunkPrologue {
    let mut prologue = ChunkPrologue::default();
    prologue.header.set_length(32);
    prologue.header.set_type(15);
    prologue.chunk.header.set_length(30);
    prologue.chunk.header.set_type(TYPE_LOG_META);
    prologue.chunk.offset = next_offset;
    prologue.chunk.data_length = chunk_len as u32;
    prologue.chunk.log_version = 0x20251120;
    prologue.chunk.chunk_size = 4096 * chunk_size_pages;
    prologue
}
