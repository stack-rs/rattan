//! Ring buffer abstractions for packet I/O
//!
//! This module provides the memory management and high-level interfaces for the four ring types:
//!
//! - [`FillRing`]: Userspace provides free UMEM chunks to kernel (producer)
//! - [`CompRing`]: Kernel returns completed chunks to userspace (consumer)
//! - [`RxRing`]: Kernel delivers received packet descriptors (consumer)
//! - [`TxRing`]: Userspace submits packet descriptors for forwarding (producer)

use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::sys::{
    RATTAN_COMP_RING_OFFSET, RATTAN_DROP_RING_OFFSET, RATTAN_FILL_RING_OFFSET, RATTAN_RING_SIZE,
    RATTAN_RINGS_SIZE, RATTAN_RX_RING_OFFSET, RATTAN_TX_RING_OFFSET, RattanCompRing, RattanDesc,
    RattanDropRing, RattanFillRing, RattanRxRing, RattanTxRing,
};
use crate::{Error, Result};

const RING_MASK: u32 = RATTAN_RING_SIZE - 1;

// ============================================================================
// Rings - Memory management for all ring buffers
// ============================================================================

/// Ring buffers for packet descriptors
///
/// Contains four rings:
/// - RX: Kernel writes packet descriptors, userspace reads
/// - TX: Userspace writes forward requests, kernel reads
/// - FILL: Userspace provides free chunks, kernel consumes
/// - COMP: Kernel returns completed chunks, userspace recycles
pub struct Rings {
    /// Pointer to mmap'd memory
    ptr: NonNull<u8>,
    /// Total length of the mapping
    len: usize,
}

impl Rings {
    /// Allocate ring buffers
    ///
    /// The memory is page-aligned and populated immediately.
    pub fn new() -> Result<Self> {
        let len = RATTAN_RINGS_SIZE;

        // Round up to page size
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
        let len = (len + page_size - 1) & !(page_size - 1);

        // mmap anonymous memory
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
            return Err(Error::Io(std::io::Error::last_os_error()));
        }

        // Zero-initialize the memory
        unsafe {
            std::ptr::write_bytes(ptr as *mut u8, 0, len);
        }

        Ok(Self {
            ptr: NonNull::new(ptr as *mut u8).unwrap(),
            len,
        })
    }

    /// Get the address of the rings memory
    #[inline]
    pub fn addr(&self) -> u64 {
        self.ptr.as_ptr() as u64
    }

    /// Get the total length of the rings memory in bytes
    #[inline]
    #[allow(clippy::len_without_is_empty)] // Rings are never "empty" - they're fixed-size
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    fn rx_ptr(&self) -> *mut RattanRxRing {
        unsafe { self.ptr.as_ptr().add(RATTAN_RX_RING_OFFSET) as *mut RattanRxRing }
    }

    #[inline]
    fn tx_ptr(&self) -> *mut RattanTxRing {
        unsafe { self.ptr.as_ptr().add(RATTAN_TX_RING_OFFSET) as *mut RattanTxRing }
    }

    #[inline]
    fn fill_ptr(&self) -> *mut RattanFillRing {
        unsafe { self.ptr.as_ptr().add(RATTAN_FILL_RING_OFFSET) as *mut RattanFillRing }
    }

    #[inline]
    fn drop_ptr(&self) -> *mut RattanDropRing {
        unsafe { self.ptr.as_ptr().add(RATTAN_DROP_RING_OFFSET) as *mut RattanDropRing }
    }

    #[inline]
    fn comp_ptr(&self) -> *mut RattanCompRing {
        unsafe { self.ptr.as_ptr().add(RATTAN_COMP_RING_OFFSET) as *mut RattanCompRing }
    }

    /// Split into individual ring handles, consuming the Rings struct
    ///
    /// Returns an `Arc<Rings>` along with the four ring handles. Each ring handle
    /// holds an `Arc` reference to the underlying memory, so the memory is
    /// automatically freed when all handles (and the returned Arc) are dropped.
    ///
    /// The ring handles can be freely moved to different threads.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rvnic::Rings;
    /// let rings = Rings::new()?;
    /// let (arc, mut fill, mut comp, mut rx, mut tx) = rings.split();
    ///
    /// // Provide chunks to kernel
    /// fill.produce(&[0, 2048, 4096]);
    ///
    /// // Process received packets
    /// let mut descs = [Default::default(); 32];
    /// let n = rx.consume(&mut descs);
    /// # Ok::<(), rvnic::Error>(())
    /// ```
    #[inline]
    pub fn split(self) -> (Arc<Rings>, FillRing, DropRing, CompRing, RxRing, TxRing) {
        let arc = Arc::new(self);
        (
            Arc::clone(&arc),
            FillRing::new(arc.fill_ptr(), Arc::clone(&arc)),
            DropRing::new(arc.drop_ptr(), Arc::clone(&arc)),
            CompRing::new(arc.comp_ptr(), Arc::clone(&arc)),
            RxRing::new(arc.rx_ptr(), Arc::clone(&arc)),
            TxRing::new(arc.tx_ptr(), arc),
        )
    }
}

impl Drop for Rings {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr.as_ptr() as *mut libc::c_void, self.len);
        }
    }
}

// SAFETY: Rings can be sent between threads (the memory is owned).
// Rings is Sync because after split(), each ring handle accesses a different
// region of the memory.
unsafe impl Send for Rings {}
unsafe impl Sync for Rings {}

// ============================================================================
// Fill Ring operations (shared between owned and borrowed versions)
// ============================================================================

#[inline]
fn fill_produce(ring: *mut RattanFillRing, addrs: &[u64]) -> usize {
    if addrs.is_empty() {
        return 0;
    }

    unsafe {
        let ring = &mut *ring;
        let prod_ptr = &ring.ring.producer as *const u32 as *const AtomicU32;
        let cons_ptr = &ring.ring.consumer as *const u32 as *const AtomicU32;

        let prod = (*prod_ptr).load(Ordering::Relaxed);
        let cons = (*cons_ptr).load(Ordering::Acquire);

        let free_slots = RATTAN_RING_SIZE.wrapping_sub(prod.wrapping_sub(cons)) as usize;
        let n = addrs.len().min(free_slots);
        if n == 0 {
            return 0;
        }

        let mut idx = prod;
        for &addr in &addrs[..n] {
            ring.addrs[(idx & RING_MASK) as usize] = addr;
            idx = idx.wrapping_add(1);
        }

        (*prod_ptr).store(prod.wrapping_add(n as u32), Ordering::Release);
        n
    }
}

#[inline]
fn fill_available(ring: *mut RattanFillRing) -> u32 {
    unsafe {
        let ring = &*ring;
        let prod_ptr = &ring.ring.producer as *const u32 as *const AtomicU32;
        let cons_ptr = &ring.ring.consumer as *const u32 as *const AtomicU32;

        let prod = (*prod_ptr).load(Ordering::Relaxed);
        let cons = (*cons_ptr).load(Ordering::Acquire);
        RATTAN_RING_SIZE.wrapping_sub(prod.wrapping_sub(cons))
    }
}

// ============================================================================
// Drop Ring operations (shared between owned and borrowed versions)
// ============================================================================

#[inline]
fn drop_produce(ring: *mut RattanDropRing, tokens: &[u64]) -> usize {
    if tokens.is_empty() {
        return 0;
    }

    unsafe {
        let ring = &mut *ring;
        let prod_ptr = &ring.ring.producer as *const u32 as *const AtomicU32;
        let cons_ptr = &ring.ring.consumer as *const u32 as *const AtomicU32;

        let prod = (*prod_ptr).load(Ordering::Relaxed);
        let cons = (*cons_ptr).load(Ordering::Acquire);

        let free_slots = RATTAN_RING_SIZE.wrapping_sub(prod.wrapping_sub(cons)) as usize;
        let n = tokens.len().min(free_slots);
        if n == 0 {
            return 0;
        }

        let mut idx = prod;
        for &addr in &tokens[..n] {
            ring.tokens[(idx & RING_MASK) as usize] = addr;
            idx = idx.wrapping_add(1);
        }

        (*prod_ptr).store(prod.wrapping_add(n as u32), Ordering::Release);
        n
    }
}

#[inline]
fn drop_available(ring: *mut RattanDropRing) -> u32 {
    unsafe {
        let ring = &*ring;
        let prod_ptr = &ring.ring.producer as *const u32 as *const AtomicU32;
        let cons_ptr = &ring.ring.consumer as *const u32 as *const AtomicU32;

        let prod = (*prod_ptr).load(Ordering::Relaxed);
        let cons = (*cons_ptr).load(Ordering::Acquire);
        RATTAN_RING_SIZE.wrapping_sub(prod.wrapping_sub(cons))
    }
}

// ============================================================================
// Comp Ring operations
// ============================================================================

#[inline]
fn comp_consume(ring: *mut RattanCompRing, addrs: &mut [u64]) -> usize {
    if addrs.is_empty() {
        return 0;
    }

    unsafe {
        let ring = &*ring;
        let prod_ptr = &ring.ring.producer as *const u32 as *const AtomicU32;
        let cons_ptr = &ring.ring.consumer as *const u32 as *const AtomicU32;

        let prod = (*prod_ptr).load(Ordering::Acquire);
        let cons = (*cons_ptr).load(Ordering::Relaxed);

        let avail = prod.wrapping_sub(cons) as usize;
        let n = addrs.len().min(avail);
        if n == 0 {
            return 0;
        }

        let mut idx = cons;
        for addr in &mut addrs[..n] {
            *addr = ring.addrs[(idx & RING_MASK) as usize];
            idx = idx.wrapping_add(1);
        }

        (*cons_ptr).store(cons.wrapping_add(n as u32), Ordering::Release);
        n
    }
}

#[inline]
fn comp_available(ring: *mut RattanCompRing) -> u32 {
    unsafe {
        let ring = &*ring;
        let prod_ptr = &ring.ring.producer as *const u32 as *const AtomicU32;
        let cons_ptr = &ring.ring.consumer as *const u32 as *const AtomicU32;

        let prod = (*prod_ptr).load(Ordering::Acquire);
        let cons = (*cons_ptr).load(Ordering::Relaxed);
        prod.wrapping_sub(cons)
    }
}

// ============================================================================
// RX Ring operations
// ============================================================================

#[inline]
fn rx_consume(ring: *mut RattanRxRing, descs: &mut [RattanDesc]) -> usize {
    if descs.is_empty() {
        return 0;
    }

    unsafe {
        let ring = &*ring;
        let prod_ptr = &ring.ring.producer as *const u32 as *const AtomicU32;
        let cons_ptr = &ring.ring.consumer as *const u32 as *const AtomicU32;

        let prod = (*prod_ptr).load(Ordering::Acquire);
        let cons = (*cons_ptr).load(Ordering::Relaxed);

        let avail = prod.wrapping_sub(cons) as usize;
        let n = descs.len().min(avail);
        if n == 0 {
            return 0;
        }

        let mut idx = cons;
        for desc in &mut descs[..n] {
            *desc = ring.descs[(idx & RING_MASK) as usize];
            idx = idx.wrapping_add(1);
        }

        (*cons_ptr).store(cons.wrapping_add(n as u32), Ordering::Release);
        n
    }
}

#[inline]
fn rx_available(ring: *mut RattanRxRing) -> u32 {
    unsafe {
        let ring = &*ring;
        let prod_ptr = &ring.ring.producer as *const u32 as *const AtomicU32;
        let cons_ptr = &ring.ring.consumer as *const u32 as *const AtomicU32;

        let prod = (*prod_ptr).load(Ordering::Acquire);
        let cons = (*cons_ptr).load(Ordering::Relaxed);
        prod.wrapping_sub(cons)
    }
}

// ============================================================================
// TX Ring operations
// ============================================================================

#[inline]
fn tx_produce(ring: *mut RattanTxRing, descs: &[RattanDesc]) -> usize {
    if descs.is_empty() {
        return 0;
    }

    unsafe {
        let ring = &mut *ring;
        let prod_ptr = &ring.ring.producer as *const u32 as *const AtomicU32;
        let cons_ptr = &ring.ring.consumer as *const u32 as *const AtomicU32;

        let prod = (*prod_ptr).load(Ordering::Relaxed);
        let cons = (*cons_ptr).load(Ordering::Acquire);

        let free_slots = RATTAN_RING_SIZE.wrapping_sub(prod.wrapping_sub(cons)) as usize;
        let n = descs.len().min(free_slots);
        if n == 0 {
            return 0;
        }

        let mut idx = prod;
        for desc in &descs[..n] {
            ring.descs[(idx & RING_MASK) as usize] = *desc;
            idx = idx.wrapping_add(1);
        }

        (*prod_ptr).store(prod.wrapping_add(n as u32), Ordering::Release);
        n
    }
}

#[inline]
fn tx_available(ring: *mut RattanTxRing) -> u32 {
    unsafe {
        let ring = &*ring;
        let prod_ptr = &ring.ring.producer as *const u32 as *const AtomicU32;
        let cons_ptr = &ring.ring.consumer as *const u32 as *const AtomicU32;

        let prod = (*prod_ptr).load(Ordering::Relaxed);
        let cons = (*cons_ptr).load(Ordering::Acquire);
        RATTAN_RING_SIZE.wrapping_sub(prod.wrapping_sub(cons))
    }
}

// ============================================================================
// FillRing - Owned version (holds Arc<Rings>)
// ============================================================================

/// Fill Ring - Userspace provides free UMEM chunks to kernel
///
/// This is the owned version that holds an `Arc<Rings>` reference.
/// The underlying memory is freed when all ring handles are dropped.
pub struct FillRing {
    ring: *mut RattanFillRing,
    _owner: Arc<Rings>,
}

impl FillRing {
    fn new(ring: *mut RattanFillRing, owner: Arc<Rings>) -> Self {
        Self {
            ring,
            _owner: owner,
        }
    }

    /// Produce chunk addresses to the fill ring
    #[inline]
    pub fn produce(&mut self, addrs: &[u64]) -> usize {
        fill_produce(self.ring, addrs)
    }

    /// Get available slots
    #[inline]
    pub fn available(&self) -> u32 {
        fill_available(self.ring)
    }
}

unsafe impl Send for FillRing {}

// ============================================================================
// DropRing - Owned version (holds Arc<Rings>)
// ============================================================================

/// Drop Ring - User space drops
///
/// This is the owned version that holds an `Arc<Rings>` reference.
/// The underlying memory is freed when all ring handles are dropped.
pub struct DropRing {
    ring: *mut RattanDropRing,
    _owner: Arc<Rings>,
}

impl DropRing {
    fn new(ring: *mut RattanDropRing, owner: Arc<Rings>) -> Self {
        Self {
            ring,
            _owner: owner,
        }
    }

    /// Produce chunk addresses to the fill ring
    #[inline]
    pub fn produce(&mut self, addrs: &[u64]) -> usize {
        drop_produce(self.ring, addrs)
    }

    /// Get available slots
    #[inline]
    pub fn available(&self) -> u32 {
        drop_available(self.ring)
    }
}

unsafe impl Send for DropRing {}

// ============================================================================
// CompRing - Owned version
// ============================================================================

/// Completion Ring - Kernel returns completed chunks to userspace
pub struct CompRing {
    ring: *mut RattanCompRing,
    _owner: Arc<Rings>,
}

impl CompRing {
    fn new(ring: *mut RattanCompRing, owner: Arc<Rings>) -> Self {
        Self {
            ring,
            _owner: owner,
        }
    }

    /// Consume completed chunk addresses from the ring
    #[inline]
    pub fn consume(&mut self, addrs: &mut [u64]) -> usize {
        comp_consume(self.ring, addrs)
    }

    /// Get available items
    #[inline]
    pub fn available(&self) -> u32 {
        comp_available(self.ring)
    }
}

unsafe impl Send for CompRing {}

// ============================================================================
// RxRing - Owned version
// ============================================================================

/// RX Ring - Kernel delivers received packet descriptors
pub struct RxRing {
    ring: *mut RattanRxRing,
    _owner: Arc<Rings>,
}

impl RxRing {
    fn new(ring: *mut RattanRxRing, owner: Arc<Rings>) -> Self {
        Self {
            ring,
            _owner: owner,
        }
    }

    /// Consume received packet descriptors from the ring
    #[inline]
    pub fn consume(&mut self, descs: &mut [RattanDesc]) -> usize {
        rx_consume(self.ring, descs)
    }

    /// Get available items
    #[inline]
    pub fn available(&self) -> u32 {
        rx_available(self.ring)
    }
}

unsafe impl Send for RxRing {}

// ============================================================================
// TxRing - Owned version
// ============================================================================

/// TX Ring - Userspace submits packet descriptors for forwarding
pub struct TxRing {
    ring: *mut RattanTxRing,
    _owner: Arc<Rings>,
}

impl TxRing {
    fn new(ring: *mut RattanTxRing, owner: Arc<Rings>) -> Self {
        Self {
            ring,
            _owner: owner,
        }
    }

    /// Produce packet descriptors to the TX ring for forwarding
    #[inline]
    pub fn produce(&mut self, descs: &[RattanDesc]) -> usize {
        tx_produce(self.ring, descs)
    }

    /// Get available slots
    #[inline]
    pub fn available(&self) -> u32 {
        tx_available(self.ring)
    }
}

unsafe impl Send for TxRing {}
