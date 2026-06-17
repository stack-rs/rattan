//! UMEM - Userspace Memory Pool
//!
//! UMEM is a contiguous memory region allocated by userspace and registered
//! with the kernel. The region is divided into fixed-size chunks that are
//! used for packet data.
//!
//! # Example
//!
//! ```no_run
//! use rvnic::{Umem, UmemBuilder};
//!
//! let umem = UmemBuilder::new()
//!     .chunk_size(2048)
//!     .headroom(128)
//!     .num_chunks(4096)
//!     .build()
//!     .expect("failed to allocate UMEM");
//!
//! println!("UMEM: {} chunks of {} bytes", umem.num_chunks(), umem.chunk_size());
//! ```

use std::io;
use std::ptr::NonNull;

use crate::Result;
use crate::sys::{RATTAN_DEFAULT_CHUNK_SIZE, RATTAN_MAX_CHUNK_SIZE, RATTAN_MIN_CHUNK_SIZE};

/// Builder for creating UMEM regions
#[derive(Debug, Clone)]
pub struct UmemBuilder {
    chunk_size: u32,
    headroom: u32,
    num_chunks: u32,
}

impl Default for UmemBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl UmemBuilder {
    /// Create a new UMEM builder with default settings
    pub fn new() -> Self {
        Self {
            chunk_size: RATTAN_DEFAULT_CHUNK_SIZE,
            headroom: 0,
            num_chunks: 4096,
        }
    }

    /// Set the chunk size (must be power of 2, 256-65536)
    pub fn chunk_size(mut self, size: u32) -> Self {
        self.chunk_size = size;
        self
    }

    /// Set the headroom before packet data in each chunk
    pub fn headroom(mut self, headroom: u32) -> Self {
        self.headroom = headroom;
        self
    }

    /// Set the number of chunks
    pub fn num_chunks(mut self, count: u32) -> Self {
        self.num_chunks = count;
        self
    }

    /// Build the UMEM region
    pub fn build(self) -> Result<Umem> {
        Umem::new(self.chunk_size, self.headroom, self.num_chunks)
    }
}

/// UMEM - Userspace Memory Pool
///
/// A contiguous memory region divided into fixed-size chunks for packet data.
/// The memory is allocated via mmap and will be pinned by the kernel when
/// registered with a device.
pub struct Umem {
    /// Pointer to the mmap'd memory
    ptr: NonNull<u8>,
    /// Total size of the memory region
    len: usize,
    /// Size of each chunk
    chunk_size: u32,
    /// Headroom before packet data
    headroom: u32,
    /// Number of chunks
    num_chunks: u32,
}

impl Umem {
    /// Create a new UMEM region
    ///
    /// # Arguments
    /// - `chunk_size`: Size of each chunk (must be power of 2, 256-65536)
    /// - `headroom`: Headroom before packet data in each chunk
    /// - `num_chunks`: Number of chunks to allocate
    ///
    /// # Errors
    /// Returns an error if:
    /// - `chunk_size` is not a power of 2
    /// - `chunk_size` is outside the valid range (256-65536)
    /// - `headroom` >= `chunk_size`
    /// - `num_chunks` is 0
    /// - mmap fails
    pub fn new(chunk_size: u32, headroom: u32, num_chunks: u32) -> Result<Self> {
        // Validate chunk_size
        if !chunk_size.is_power_of_two() {
            return Err(crate::Error::InvalidParam("chunk_size must be power of 2"));
        }
        if !(RATTAN_MIN_CHUNK_SIZE..=RATTAN_MAX_CHUNK_SIZE).contains(&chunk_size) {
            return Err(crate::Error::InvalidParam("chunk_size must be 256-65536"));
        }

        // Validate headroom
        if headroom >= chunk_size {
            return Err(crate::Error::InvalidParam("headroom must be < chunk_size"));
        }

        // Validate num_chunks
        if num_chunks == 0 {
            return Err(crate::Error::InvalidParam("num_chunks must be > 0"));
        }

        let len_u64 = (chunk_size as u64)
            .checked_mul(num_chunks as u64)
            .ok_or(crate::Error::InvalidParam("UMEM size overflow"))?;
        if len_u64 > crate::sys::RATTAN_MAX_UMEM_SIZE {
            return Err(crate::Error::InvalidParam(
                "UMEM size exceeds RATTAN_MAX_UMEM_SIZE",
            ));
        }
        if len_u64 > usize::MAX as u64 {
            return Err(crate::Error::InvalidParam(
                "UMEM too large for this platform",
            ));
        }
        let len = len_u64 as usize;

        // Allocate memory via mmap
        // MAP_POPULATE pre-faults the pages to avoid page faults during operation
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_POPULATE,
                -1,
                0,
            )
        };

        if ptr == libc::MAP_FAILED {
            return Err(crate::Error::Io(io::Error::last_os_error()));
        }

        Ok(Self {
            ptr: NonNull::new(ptr as *mut u8).expect("mmap returned null"),
            len,
            chunk_size,
            headroom,
            num_chunks,
        })
    }

    /// Get the base address of the UMEM region
    #[inline]
    pub fn addr(&self) -> u64 {
        self.ptr.as_ptr() as u64
    }

    /// Get the total length of the UMEM region in bytes
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if UMEM is empty (always false for valid UMEM)
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get the chunk size in bytes
    #[inline]
    pub fn chunk_size(&self) -> u32 {
        self.chunk_size
    }

    /// Get the headroom in bytes
    #[inline]
    pub fn headroom(&self) -> u32 {
        self.headroom
    }

    /// Get the number of chunks
    #[inline]
    pub fn num_chunks(&self) -> u32 {
        self.num_chunks
    }

    /// Get a raw pointer to a chunk by index
    ///
    /// Returns `None` if the index is out of bounds.
    #[inline]
    pub fn chunk_ptr(&self, index: u32) -> Option<*mut u8> {
        if index >= self.num_chunks {
            return None;
        }
        let offset = index as usize * self.chunk_size as usize;
        Some(unsafe { self.ptr.as_ptr().add(offset) })
    }

    /// Get a raw pointer to the data area of a chunk (after headroom)
    ///
    /// Returns `None` if the index is out of bounds.
    #[inline]
    pub fn data_ptr(&self, index: u32) -> Option<*mut u8> {
        self.chunk_ptr(index)
            .map(|p| unsafe { p.add(self.headroom as usize) })
    }

    /// Get the offset of a chunk from the UMEM base address
    #[inline]
    pub fn chunk_offset(&self, index: u32) -> Option<u64> {
        if index >= self.num_chunks {
            return None;
        }
        Some(index as u64 * self.chunk_size as u64)
    }

    /// Convert an offset to a chunk index
    ///
    /// Returns `None` if the offset is not chunk-aligned or out of bounds.
    #[inline]
    pub fn offset_to_index(&self, offset: u64) -> Option<u32> {
        // Check alignment (chunk_size is power of 2)
        if offset & (self.chunk_size as u64 - 1) != 0 {
            return None;
        }
        let index = offset / self.chunk_size as u64;
        if index >= self.num_chunks as u64 {
            return None;
        }
        Some(index as u32)
    }
}

impl Drop for Umem {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr.as_ptr() as *mut libc::c_void, self.len);
        }
    }
}

// SAFETY: Umem is just a pointer to mmap'd memory with no interior mutability
// concerns beyond what the raw pointer provides
unsafe impl Send for Umem {}
unsafe impl Sync for Umem {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_umem_builder_default() {
        let builder = UmemBuilder::new();
        assert_eq!(builder.chunk_size, 256);
        assert_eq!(builder.headroom, 0);
        assert_eq!(builder.num_chunks, 4096);
    }

    #[test]
    fn test_umem_builder_custom() {
        let builder = UmemBuilder::new()
            .chunk_size(4096)
            .headroom(256)
            .num_chunks(1024);
        assert_eq!(builder.chunk_size, 4096);
        assert_eq!(builder.headroom, 256);
        assert_eq!(builder.num_chunks, 1024);
    }

    #[test]
    fn test_umem_creation() {
        let umem = Umem::new(2048, 128, 64).expect("failed to create UMEM");
        assert_eq!(umem.chunk_size(), 2048);
        assert_eq!(umem.headroom(), 128);
        assert_eq!(umem.num_chunks(), 64);
        assert_eq!(umem.len(), 2048 * 64);
    }

    #[test]
    fn test_umem_chunk_access() {
        let umem = Umem::new(2048, 128, 64).expect("failed to create UMEM");

        // Valid indices
        assert!(umem.chunk_ptr(0).is_some());
        assert!(umem.chunk_ptr(63).is_some());

        // Invalid index
        assert!(umem.chunk_ptr(64).is_none());

        // Check offsets
        assert_eq!(umem.chunk_offset(0), Some(0));
        assert_eq!(umem.chunk_offset(1), Some(2048));
        assert_eq!(umem.chunk_offset(63), Some(63 * 2048));
    }

    #[test]
    fn test_umem_offset_to_index() {
        let umem = Umem::new(2048, 0, 64).expect("failed to create UMEM");

        // Valid offsets
        assert_eq!(umem.offset_to_index(0), Some(0));
        assert_eq!(umem.offset_to_index(2048), Some(1));
        assert_eq!(umem.offset_to_index(63 * 2048), Some(63));

        // Unaligned offset
        assert_eq!(umem.offset_to_index(100), None);

        // Out of bounds
        assert_eq!(umem.offset_to_index(64 * 2048), None);
    }

    #[test]
    fn test_umem_invalid_params() {
        // Not power of 2
        assert!(Umem::new(1000, 0, 64).is_err());

        // Too small
        assert!(Umem::new(128, 0, 64).is_err());

        // Too large
        assert!(Umem::new(128 * 1024, 0, 64).is_err());

        // Headroom >= chunk_size
        assert!(Umem::new(2048, 2048, 64).is_err());

        // Zero chunks
        assert!(Umem::new(2048, 0, 0).is_err());
    }
}
