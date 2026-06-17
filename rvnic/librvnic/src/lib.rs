//! # rvnic - Rattan Virtual NIC Userspace Library
//!
//! This crate provides a safe Rust interface to the `rattan-vnic` kernel module.
//!
//! It supports both:
//! - simple single-queue setups via queue-0 convenience helpers
//! - multiqueue setups with one ring set per queue
//!
//! ## Quick Start
//!
//! ```no_run
//! use rvnic::{RvnicDevice, UmemBuilder, Rings};
//!
//! // Open device (creates rattan0 interface)
//! let mut dev = RvnicDevice::open()?;
//!
//! // Allocate UMEM (128KB = 64 chunks × 2KB)
//! let umem = UmemBuilder::new()
//!     .chunk_size(2048)
//!     .headroom(128)
//!     .num_chunks(64)
//!     .build()?;
//!
//! // Allocate rings
//! let rings = Rings::new()?;
//!
//! // Register UMEM and queue-0 rings with kernel
//! dev.register_umem(umem)?;
//! dev.register_rings_for_queue(0, rings)?;
//!
//! // Start the device (napi_cpu = -1 means use caller's CPU)
//! dev.start(-1)?;
//!
//! // ... process packets ...
//!
//! // Cleanup happens automatically on drop
//! # Ok::<(), rvnic::Error>(())
//! ```
//!
//! ## Multiqueue Setup
//!
//! ```no_run
//! use rvnic::{QueueConfig, Rings, RvnicDevice, UmemBuilder};
//!
//! let mut dev = RvnicDevice::open()?;
//! let umem = UmemBuilder::new().build()?;
//! dev.register_umem(umem)?;
//!
//! let queues = vec![
//!     QueueConfig {
//!         queue_id: 0,
//!         rings: Rings::new()?,
//!     },
//!     QueueConfig {
//!         queue_id: 1,
//!         rings: Rings::new()?,
//!     },
//! ];
//!
//! let mut queue_rings = dev.register_queues(queues)?;
//! dev.start(-1)?;
//!
//! // Kick queue 0
//! let _processed = dev.kick_rx_queue(queue_rings[0].queue_id)?;
//!
//! // Access per-queue rings
//! let q0 = &mut queue_rings[0];
//! let _free_slots = q0.fill.available();
//! let _pending_rx = q0.rx.available();
//! # Ok::<(), rvnic::Error>(())
//! ```

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

mod device;
mod rings;
mod umem;

// Low-level types (public for advanced users)
pub mod sys;

// Re-export public types
pub use device::{DEVICE_PATH, QueueConfig, QueueRings, RvnicDevice};
pub use rings::{CompRing, DropRing, FillRing, Rings, RxRing, TxRing};
pub use umem::{Umem, UmemBuilder};

// Re-export constants that users might need
pub use sys::{
    RATTAN_DEFAULT_CHUNK_SIZE, RATTAN_HEADER_SIZE, RATTAN_MAX_CHUNK_SIZE, RATTAN_MIN_CHUNK_SIZE,
    RATTAN_RING_SIZE,
};

use std::io;

/// Error type for rvnic operations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error (file operations, ioctl failures)
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// UMEM is already registered on this device
    #[error("UMEM already registered")]
    UmemAlreadyRegistered,

    /// Rings are already registered on this device
    #[error("rings already registered")]
    RingsAlreadyRegistered,

    /// Device is not properly configured
    #[error("device not configured: {0}")]
    NotConfigured(&'static str),

    /// Device is already started
    #[error("device already started")]
    AlreadyStarted,

    /// Invalid parameter provided
    #[error("invalid parameter: {0}")]
    InvalidParam(&'static str),
}

/// Result type for rvnic operations
pub type Result<T> = std::result::Result<T, Error>;
