use derive_more::{Deref, DerefMut};
use memmap2::{Mmap, MmapMut, MmapOptions};

use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Result};

use std::path::PathBuf;

pub const PAGE_SIZE: u64 = 1 << 12;

pub fn mmap_mut_segment(file: &File, offset: u64, length: usize) -> Result<MmapMut> {
    let metadata = file.metadata()?;
    let file_size = metadata.len();
    if offset % PAGE_SIZE != 0 {
        return Err(Error::new(ErrorKind::InvalidInput, "Unaligned offset!"));
    }

    let end = offset
        .checked_add(length as u64)
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Offset + length overflow"))?;

    if end > file_size {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            format!("Requested mapping [{offset}..{end}) exceeds file size ({file_size})"),
        ));
    }

    let mmap = unsafe {
        MmapOptions::new()
            .offset(offset)
            .len(length)
            .map_mut(file)?
    };
    Ok(mmap)
}

pub fn mmap_file(file: &File) -> Result<Mmap> {
    unsafe { MmapOptions::new().map(file) }
}

pub struct MmapChunk {
    mmap: MmapMut,
    length: usize,
    written: usize,
}

impl MmapChunk {
    #[inline(always)]
    fn remain(&self) -> usize {
        self.length.saturating_sub(self.written)
    }

    #[inline(always)]
    fn append(&mut self, slice: &[u8]) -> usize {
        self.write_at(slice, self.written)
    }

    #[inline(always)]
    fn write_at(&mut self, slice: &[u8], offset: usize) -> usize {
        let end = offset + slice.len();
        if let Some(to_write) = self.mmap.get_mut(offset..end) {
            to_write.copy_from_slice(slice);
            self.written = self.written.max(end);
            slice.len()
        } else {
            0
        }
    }

    #[inline(always)]
    fn advance(&mut self, offset: usize) -> usize {
        let advance_to = (self.written + offset).min(self.length);
        self.written = advance_to;
        self.written
    }

    fn new(file: &File, offset: u64, length: usize) -> Result<Self> {
        Ok(Self {
            mmap: mmap_mut_segment(file, offset, length)?,
            length,
            written: 0,
        })
    }
}

pub struct MmapWritter<const P: usize> {
    file: File,
    path: PathBuf,
    chunk_start: u64,
    chunk: MmapChunk,
}

impl<const P: usize> Drop for MmapWritter<P> {
    fn drop(&mut self) {
        let file_last_write = self.offset();
        let _ = self.file.set_len(file_last_write);
        tracing::info!(
            "Written {} Bytes into {:?}",
            file_last_write,
            self.path.display()
        );
    }
}

impl<const P: usize> MmapWritter<P> {
    #[inline(always)]
    fn page() -> usize {
        P * PAGE_SIZE as usize
    }

    fn offset_in_chunk(&self) -> usize {
        self.chunk.written
    }

    fn offset(&self) -> u64 {
        self.chunk_start + self.chunk.written as u64
    }

    fn renew_chunk(&mut self) -> Result<()> {
        let new_chunk_start = self.chunk_start + Self::page() as u64;
        let new_file_size = new_chunk_start + Self::page() as u64;

        self.file.set_len(new_file_size)?;
        tracing::debug!("New chunk at [{}, {})", new_chunk_start, new_file_size);
        self.chunk = MmapChunk::new(&self.file, new_chunk_start, Self::page())?;
        self.chunk_start = new_chunk_start;
        Ok(())
    }

    pub fn new_truncate(path: &PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(path)?;
        file.set_len(Self::page() as u64)?;
        let chunk = MmapChunk::new(&file, 0, Self::page())?;
        Ok(Self {
            file,
            path: path.clone(),
            chunk_start: 0,
            chunk,
        })
    }
}

pub struct MmapChunkWriter<const P: usize, H, C>
where
    H: FnMut(usize, &Option<C>) -> Vec<u8>,
{
    writer: MmapWritter<P>,
    prologue_maker: H,
    prologue_info: Option<C>,
    prologue_len: usize,
    logical_chunk_len: usize,
    current_chunk_offset: usize,
    init: bool,
}

impl<const P: usize, H, C> Drop for MmapChunkWriter<P, H, C>
where
    H: FnMut(usize, &Option<C>) -> Vec<u8>,
{
    fn drop(&mut self) {
        self.finish_chunk(true);
        tracing::info!("Mmap total size: {:?}", self.writer.offset());
    }
}

impl<const P: usize, H, C> MmapChunkWriter<P, H, C>
where
    H: FnMut(usize, &Option<C>) -> Vec<u8>,
{
    fn finish_chunk(&mut self, is_final: bool) {
        let current_chunk_len = self
            .writer
            .offset_in_chunk()
            .saturating_sub(self.current_chunk_offset);
        if current_chunk_len <= self.prologue_len {
            tracing::debug!("Empty chunk");
            return;
        }
        let header = (self.prologue_maker)(current_chunk_len, &self.prologue_info);
        assert_eq!(header.len(), self.prologue_len);
        tracing::debug!(
            "Finishing current logic trunc at {} + [{},{}). chunk len {}",
            self.writer.chunk_start,
            self.current_chunk_offset,
            self.current_chunk_offset + current_chunk_len,
            current_chunk_len
        );
        assert_eq!(
            self.prologue_len,
            self.writer
                .chunk
                .write_at(&header, self.current_chunk_offset)
        );
        if is_final {
            return;
        }
        let tail_space = self.logical_chunk_len - current_chunk_len;
        self.writer.chunk.advance(tail_space);
        debug_assert_eq!(
            self.writer.offset_in_chunk(),
            self.current_chunk_offset + self.logical_chunk_len
        );
        self.current_chunk_offset += self.logical_chunk_len;
    }

    fn renew_chunk(&mut self) -> Result<()> {
        self.init = true;
        self.finish_chunk(false);

        if self.current_chunk_offset >= MmapWritter::<P>::page() {
            self.writer.renew_chunk()?;
            self.current_chunk_offset = 0;
        }

        tracing::info!(
            "Start recording at {} + [{}, {})",
            self.writer.chunk_start,
            self.current_chunk_offset,
            self.current_chunk_offset + self.logical_chunk_len
        );

        // Allocate space at the front of logical chunk for chunk prologue
        self.writer.chunk.advance(self.prologue_len);
        debug_assert_eq!(
            self.writer.offset_in_chunk(),
            self.current_chunk_offset + self.prologue_len
        );
        Ok(())
    }

    pub fn remain(&self) -> usize {
        if !self.init {
            return 0;
        }
        self.logical_chunk_len - (self.writer.offset_in_chunk() - self.current_chunk_offset)
    }

    fn record_slice(&mut self, slice: &[u8], info: Option<C>) {
        if let Some(info) = info {
            let _ = self.prologue_info.insert(info);
        }

        let start_offset = self.writer.offset_in_chunk();

        // Make sure `remain` is larger than slice.len to avoid panic
        assert_eq!(slice.len(), self.writer.chunk.append(slice));

        tracing::trace!(
            "Recorded at  {} + [{}, {})",
            self.writer.chunk_start,
            start_offset,
            self.writer.offset_in_chunk()
        );
    }

    pub fn new_truncate(
        path: &PathBuf,
        header_maker: H,
        prologue_len: usize,
        logical_chunk_len: usize,
    ) -> Result<Self> {
        assert_eq!(MmapWritter::<P>::page() % logical_chunk_len, 0);
        assert!(prologue_len < logical_chunk_len);

        let writer = MmapWritter::new_truncate(path)?;
        Ok(Self {
            writer,
            prologue_maker: header_maker,
            prologue_len,
            logical_chunk_len,
            current_chunk_offset: 0,
            init: false,
            prologue_info: None,
        })
    }

    pub fn allocate(
        &mut self,
        len: usize,
    ) -> Result<(bool, MmapChunkWriterAllocation<'_, P, H, C>)> {
        let mut new_chunk = false;
        if self.remain() < len {
            self.renew_chunk()?;
            new_chunk = true;
        }
        if self.remain() < len {
            return Err(Error::new(
                ErrorKind::FileTooLarge,
                format!(
                    "Log entry should be at most {} Bytes, actual {} Bytes",
                    self.logical_chunk_len - self.prologue_len,
                    len
                ),
            ));
        }
        Ok((new_chunk, MmapChunkWriterAllocation { len, inner: self }))
    }
}

pub struct MmapChunkWriterAllocation<'a, const P: usize, H, C>
where
    H: FnMut(usize, &Option<C>) -> Vec<u8>,
{
    pub len: usize,
    inner: &'a mut MmapChunkWriter<P, H, C>,
}

impl<'a, const P: usize, H, C> MmapChunkWriterAllocation<'a, P, H, C>
where
    H: FnMut(usize, &Option<C>) -> Vec<u8>,
{
    pub fn consume(mut self, slice: &[u8], prologue_info: Option<C>) {
        assert_eq!(self.len, slice.len());
        self.inner.record_slice(slice, prologue_info);
        self.len = 0;
    }
}

/// A stream-like writer that uses memory-mapping (mmap) to efficiently write append-only data to a file.
///
/// Data is written directly into large, fixed-size, memory-mapped chunks to minimize
/// system calls. The constant `P` determines the chunk size as `P * PAGE_SIZE`.
///
/// When dropped, the file is automatically truncated to the actual amount of data written,
/// discarding any remaining allocated but unused space in the last chunk.
#[derive(Deref, DerefMut)]
pub struct MmapStreamWriter<const P: usize> {
    inner: MmapWritter<P>,
}

impl<const P: usize> MmapStreamWriter<P> {
    pub fn new_truncate(path: &PathBuf) -> Result<Self> {
        Ok(Self {
            inner: MmapWritter::new_truncate(path)?,
        })
    }

    /// If ok, returns the (offset, length) of the written slice.
    pub fn extend_from_slice(&mut self, slice: impl AsRef<[u8]>) -> Result<(u64, usize)> {
        let offset = self.offset();
        let mut slice = slice.as_ref();
        let len = slice.len();

        let mut written = 0;

        tracing::debug!("Trying to write [{}, {})", offset, offset + len as u64);
        while !slice.is_empty() {
            let remain = self.chunk.remain();
            let (to_write, remained) = slice.split_at(remain.min(slice.len()));
            slice = remained;

            match (to_write.is_empty(), remained.is_empty()) {
                (true, true) => {
                    break;
                }
                (true, false) => self.renew_chunk()?,
                // Safety of `unwrap`:  if self.chunk is None, `remain` would be 0 and `to_write` is always empty.
                (false, _) => written += self.chunk.append(to_write),
            }
        }

        debug_assert_eq!(len, written);

        Ok((offset, len))
    }
}

#[cfg(test)]
mod test {
    use std::io::Read;

    use super::*;

    use random::Source;
    use tempfile::tempdir;

    fn new_file(name: &str) -> Result<PathBuf> {
        let dir = tempdir()?;
        Ok(dir.path().join(name))
    }

    fn stream_writting(pattern: impl Iterator<Item = usize>) {
        let file = new_file("stream.bin").unwrap();

        let mut writter = MmapStreamWriter::<1>::new_truncate(&file).unwrap();
        let mut bytes = (0..).into_iter().map(|x| ((x * 7) % 256) as u8);

        let mut total = 0;

        for write_len in pattern {
            let mut slice = Vec::with_capacity(write_len);
            slice.resize_with(write_len, || bytes.next().unwrap());
            let (write_at, len) = writter.extend_from_slice(&slice).unwrap();
            assert_eq!(write_at, total);
            assert_eq!(len, write_len);
            total += write_len as u64;
        }

        tracing::info!("{} Bytes are expect to be written", total);
        assert_eq!(writter.offset(), total);
        drop(writter);
        let mut file = OpenOptions::new().read(true).open(file).unwrap();
        assert_eq!(file.metadata().unwrap().len(), total);

        let mut file_content = Vec::with_capacity(total as usize);

        file.read_to_end(&mut file_content).unwrap();

        // Check file content
        assert!(!file_content
            .iter()
            .enumerate()
            .any(|(i, &b)| ((i * 7) % 256) as u8 != b));
    }

    #[test_log::test]
    fn streamed_writting() {
        let mut rng = random::default(80233);

        // Random length
        stream_writting(std::iter::from_fn(|| Some((rng.read_u64() % 16 + 32) as usize)).take(128));
        stream_writting(
            std::iter::from_fn(|| Some((rng.read_u64() % 2048 + 4096) as usize)).take(128),
        );
        stream_writting(
            std::iter::from_fn(|| Some((rng.read_u64() % 16384 + 32768) as usize)).take(128),
        );

        // Aligned length
        stream_writting(std::iter::once(16).cycle().take(1024));
        stream_writting(std::iter::once(32).cycle().take(1024));
        stream_writting(std::iter::once(64).cycle().take(1024));
    }

    #[test_log::test]
    fn chunked_writing() {
        let file = new_file("chunk.bin").unwrap();
        const PROLOGUE_LENGTH: usize = 32;

        let mut header_id: u32 = 0;
        let new_header = |chunk_len: usize, run_time_data: &Option<usize>| {
            let mut header: Vec<u8> = vec![];
            header.resize(PROLOGUE_LENGTH, 7u8);
            header[0..8].copy_from_slice(&chunk_len.to_be_bytes());
            header[8..12].copy_from_slice(&header_id.to_be_bytes());
            header_id += 1;
            tracing::info!(
                "[{}]Logical chunk prologue created. {} ",
                header_id,
                chunk_len
            );
            tracing::info!(
                "This header is build for logs start form  {:?}",
                run_time_data
            );
            header
        };

        let mut writter =
            MmapChunkWriter::<8, _, usize>::new_truncate(&file, new_header, PROLOGUE_LENGTH, 8192)
                .unwrap();

        for i in 0..100 {
            let mut slice = Vec::new();
            slice.resize(1000, 2);
            let (new, allocation) = writter.allocate(slice.len()).unwrap();
            if new {
                allocation.consume(&slice, i.into());
            } else {
                allocation.consume(&slice, None);
            }

            assert_eq!(i % 8 == 0, new);
        }
    }
}
