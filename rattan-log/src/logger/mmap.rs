use memmap2::{Mmap, MmapMut, MmapOptions};

use std::fs::File;
use std::io::{Error, ErrorKind, Result};

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

pub fn mmap_segment(file: &File, offset: u64, length: usize) -> Result<Mmap> {
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

    let mmap = unsafe { MmapOptions::new().offset(offset).len(length).map(file)? };
    Ok(mmap)
}

pub fn mmap_file(file: &File) -> Result<Mmap> {
    unsafe { MmapOptions::new().map(file) }
}
