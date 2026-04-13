//! RvnicDevice - Handle to a rattan virtual NIC
//!
//! Opening a device creates a new `rattanX` network interface. The device
//! must be configured with UMEM and rings before starting.
//!
//! # Example
//!
//! ```no_run
//! use rvnic::{RvnicDevice, UmemBuilder, Rings};
//!
//! // Open device (creates rattan0 interface)
//! let mut dev = RvnicDevice::open().expect("failed to open device");
//!
//! // Allocate and register UMEM
//! let umem = UmemBuilder::new()
//!     .chunk_size(2048)
//!     .num_chunks(4096)
//!     .build()
//!     .expect("failed to allocate UMEM");
//!
//! dev.register_umem(umem).expect("failed to register UMEM");
//!
//! // Allocate and register rings
//! let rings = Rings::new().expect("failed to allocate rings");
//! dev.register_rings(rings).expect("failed to register rings");
//!
//! // Start the device (napi_cpu = -1 means use caller's CPU)
//! dev.start(-1).expect("failed to start device");
//!
//! // ... use the device ...
//!
//! // Stop the device
//! dev.stop().expect("failed to stop device");
//!
//! // Device, UMEM, and rings are cleaned up on drop
//! ```

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::Arc;

use crate::rings::{CompRing, DropRing, FillRing, Rings, RxRing, TxRing};
use crate::sys::{
    RattanPayloadReq, RattanRingsReg, RattanRingsRegQ, RattanUmemReg, ioctl_kick_rx_q,
    ioctl_read_payload, ioctl_reg_rings, ioctl_reg_rings_q, ioctl_reg_umem, ioctl_release,
    ioctl_share_umem, ioctl_start, ioctl_stop,
};
use crate::umem::Umem;
use crate::{Error, Result};

/// Path to the rattan-vnic device file
pub const DEVICE_PATH: &str = "/dev/rattan-vnic";

/// Owned ring handles for one queue.
///
/// This bundles the four queue-local rings returned after registration so
/// multiqueue applications can manage one queue at a time.
pub struct QueueRings {
    /// Queue identifier
    pub queue_id: u32,
    /// Fill ring for providing free UMEM chunks to the kernel
    pub fill: FillRing,
    /// Drop ring for release resources dropped by the userspace
    pub drop: DropRing,
    /// Completion ring for reclaiming finished UMEM chunks
    pub comp: CompRing,
    /// RX ring for consuming packet descriptors from the kernel
    pub rx: RxRing,
    /// TX ring for submitting forwarding descriptors to the kernel
    pub tx: TxRing,
}

/// Ring memory plus queue id used for batch registration.
///
/// This is useful for ergonomic multiqueue setup where userspace allocates a
/// ring set per queue first and then registers them in one call.
pub struct QueueConfig {
    /// Queue identifier
    pub queue_id: u32,
    /// Queue-local ring memory
    pub rings: Rings,
}

/// Handle to a rattan virtual NIC device
///
/// Opening this device creates a new `rattanX` network interface.
/// The interface is destroyed when this handle is dropped.
pub struct RvnicDevice {
    file: File,
    /// UMEM memory (kept alive via Arc, may be shared between devices)
    umem: Option<Arc<Umem>>,
    /// Rings memory per queue (kept alive via Arc)
    rings: BTreeMap<u32, Arc<Rings>>,
    started: bool,
}

impl RvnicDevice {
    /// Open a new rattan virtual NIC device
    ///
    /// This creates a new `rattanX` network interface (e.g., `rattan0`).
    /// The interface name is assigned by the kernel based on availability.
    ///
    /// # Errors
    /// Returns an error if:
    /// - The device file doesn't exist (module not loaded)
    /// - Permission denied
    /// - Maximum number of devices reached
    pub fn open() -> Result<Self> {
        Self::open_path(DEVICE_PATH)
    }

    /// Open a rattan virtual NIC device at a custom path
    ///
    /// This is mainly useful for testing.
    pub fn open_path(path: &str) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;

        Ok(Self {
            file,
            umem: None,
            rings: BTreeMap::new(),
            started: false,
        })
    }

    /// Get the raw file descriptor
    ///
    /// This can be used for poll/epoll or other low-level operations.
    #[inline]
    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Check if UMEM is configured
    #[inline]
    pub fn has_umem(&self) -> bool {
        self.umem.is_some()
    }

    /// Check if rings are registered
    #[inline]
    pub fn has_rings(&self) -> bool {
        self.has_rings_queue(0)
    }

    /// Check if rings are registered for a specific queue
    #[inline]
    pub fn has_rings_queue(&self, queue_id: u32) -> bool {
        self.rings.contains_key(&queue_id)
    }

    /// Check if the device is started
    #[inline]
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Get a reference to the registered UMEM
    #[inline]
    pub fn umem(&self) -> Option<&Umem> {
        self.umem.as_deref()
    }

    /// Register a UMEM region with the kernel
    ///
    /// The UMEM memory will be pinned by the kernel and used for packet data.
    /// UMEM can only be registered once per device.
    ///
    /// Returns an `Arc<Umem>` that can be used to share UMEM with other devices.
    ///
    /// # Errors
    /// Returns an error if:
    /// - UMEM is already registered
    /// - The ioctl fails
    pub fn register_umem(&mut self, umem: Umem) -> Result<Arc<Umem>> {
        if self.umem.is_some() {
            return Err(Error::UmemAlreadyRegistered);
        }

        let reg = RattanUmemReg {
            addr: umem.addr(),
            len: umem.len() as u64,
            chunk_size: umem.chunk_size(),
            headroom: umem.headroom(),
        };

        // SAFETY: fd is valid (we own the File), reg is valid on stack
        unsafe {
            ioctl_reg_umem(self.fd(), &reg)?;
        }

        let arc = Arc::new(umem);
        self.umem = Some(Arc::clone(&arc));
        Ok(arc)
    }

    /// Share UMEM from another device
    ///
    /// # Arguments
    /// * `source` - The device that owns the UMEM
    ///
    /// # Errors
    /// Returns an error if:
    /// - This device already has UMEM
    /// - Source device has no UMEM
    /// - The ioctl fails
    ///
    /// # Example
    /// ```no_run
    /// use rvnic::{RvnicDevice, UmemBuilder, Rings};
    ///
    /// let mut dev0 = RvnicDevice::open().unwrap();
    /// let umem = UmemBuilder::new().build().unwrap();
    /// dev0.register_umem(umem).unwrap();
    ///
    /// let mut dev1 = RvnicDevice::open().unwrap();
    /// dev1.share_umem(&dev0).unwrap();  // Share UMEM from dev0
    ///
    /// // Both devices can now use the same UMEM
    /// // Each still needs its own rings
    /// ```
    pub fn share_umem(&mut self, source: &RvnicDevice) -> Result<()> {
        if self.umem.is_some() {
            return Err(Error::UmemAlreadyRegistered);
        }

        let source_umem = source
            .umem
            .as_ref()
            .ok_or(Error::NotConfigured("source device has no UMEM"))?;

        // SAFETY: fd is valid (we own the File), source fd is valid
        unsafe {
            ioctl_share_umem(self.fd(), source.fd())?;
        }

        self.umem = Some(Arc::clone(source_umem));
        Ok(())
    }

    /// Register ring buffers with the kernel for a specific queue
    ///
    /// The rings memory will be pinned by the kernel and used for packet
    /// descriptors. Each queue can only be registered once per device.
    ///
    /// Returns the four ring handles for packet I/O operations.
    ///
    /// # Errors
    /// Returns an error if:
    /// - UMEM is not registered (must register UMEM first)
    /// - Rings are already registered for this queue
    /// - The ioctl fails
    pub fn register_rings_for_queue(
        &mut self,
        queue_id: u32,
        rings: Rings,
    ) -> Result<(FillRing, DropRing, RxRing, TxRing)> {
        let queue = self.register_queue(queue_id, rings)?;
        // In current version, user space should not use the comp ring
        Ok((queue.fill, queue.drop, queue.rx, queue.tx))
    }

    /// Register ring buffers and return an ergonomic queue bundle
    ///
    /// This is the queue-oriented variant of [`Self::register_rings_for_queue`].
    /// It returns a [`QueueRings`] bundle instead of a tuple.
    ///
    /// # Errors
    /// Returns an error if:
    /// - UMEM is not registered
    /// - Rings are already registered for this queue
    /// - The ioctl fails
    pub fn register_queue(&mut self, queue_id: u32, rings: Rings) -> Result<QueueRings> {
        if self.umem.is_none() {
            return Err(Error::NotConfigured("UMEM not registered"));
        }
        if self.rings.contains_key(&queue_id) {
            return Err(Error::RingsAlreadyRegistered);
        }

        if queue_id == 0 {
            let reg = RattanRingsReg {
                addr: rings.addr(),
                len: rings.len() as u64,
            };

            // SAFETY: fd is valid (we own the File), reg is valid on stack
            unsafe {
                ioctl_reg_rings(self.fd(), &reg)?;
            }
        } else {
            let reg = RattanRingsRegQ {
                addr: rings.addr(),
                len: rings.len() as u64,
                queue_id,
                _pad: 0,
            };

            // SAFETY: fd is valid (we own the File), reg is valid on stack
            unsafe {
                ioctl_reg_rings_q(self.fd(), &reg)?;
            }
        }

        let (arc, fill, drop, comp, rx, tx) = rings.split();
        self.rings.insert(queue_id, arc);
        Ok(QueueRings {
            queue_id,
            fill,
            drop,
            comp,
            rx,
            tx,
        })
    }

    /// Register multiple queues in one call
    ///
    /// The provided queue configurations are registered sequentially. If a
    /// later queue registration fails, previously registered queues remain
    /// registered.
    ///
    /// # Errors
    /// Returns an error if:
    /// - UMEM is not registered
    /// - Any queue is already registered
    /// - Any ioctl fails
    pub fn register_queues(&mut self, queues: Vec<QueueConfig>) -> Result<Vec<QueueRings>> {
        let mut registered = Vec::with_capacity(queues.len());

        for queue in queues {
            registered.push(self.register_queue(queue.queue_id, queue.rings)?);
        }

        Ok(registered)
    }

    /// Start the device
    ///
    /// This enables packet processing on the device. The network interface
    /// will show carrier on after this call.
    ///
    /// # Arguments
    /// * `napi_cpu` - Target CPU for NAPI polling, or -1 to use caller's CPU
    ///
    /// # Errors
    /// Returns an error if:
    /// - UMEM is not registered
    /// - Rings are not registered
    /// - The device is already started
    /// - The ioctl fails
    pub fn start(&mut self, napi_cpu: i32) -> Result<()> {
        if self.umem.is_none() {
            return Err(Error::NotConfigured("UMEM not registered"));
        }
        if self.rings.is_empty() {
            return Err(Error::NotConfigured("rings not registered"));
        }
        if self.started {
            return Err(Error::AlreadyStarted);
        }

        // SAFETY: fd is valid
        unsafe {
            ioctl_start(self.fd(), napi_cpu)?;
        }

        self.started = true;
        Ok(())
    }

    /// Stop the device
    ///
    /// This disables packet processing on the device. The network interface
    /// will show carrier off after this call.
    ///
    /// # Errors
    /// Returns an error if the ioctl fails
    pub fn stop(&mut self) -> Result<()> {
        if !self.started {
            return Ok(()); // Already stopped, not an error
        }

        // SAFETY: fd is valid
        unsafe {
            ioctl_stop(self.fd())?;
        }

        self.started = false;
        Ok(())
    }

    /// Read payload from a tracked packet
    ///
    /// Given a token from an RX descriptor, reads the full packet payload
    /// into the provided buffer.
    ///
    /// # Arguments
    /// * `token` - The token from the RX descriptor
    /// * `buf` - Buffer to read payload into
    /// * `offset` - Offset within the packet to start reading
    ///
    /// # Returns
    /// A tuple of (bytes_copied, total_packet_length)
    ///
    /// # Errors
    /// Returns an error if:
    /// - Token is not found (already released or invalid)
    /// - The ioctl fails
    pub fn read_payload(&self, token: u64, buf: &mut [u8], offset: u32) -> Result<(u32, u32)> {
        let req = RattanPayloadReq {
            token,
            buf: buf.as_mut_ptr() as u64,
            len: buf.len() as u32,
            offset,
        };

        // SAFETY: fd is valid, buf is valid for the length specified
        let resp = unsafe { ioctl_read_payload(self.fd(), &req)? };

        Ok((resp.len, resp.total_len))
    }

    /// Release a token - free the SKB in kernel
    ///
    /// # Arguments
    /// * `token` - The token to release
    ///
    /// # Errors
    /// Returns an error if:
    /// - Token is not found (already released or invalid)
    /// - The ioctl fails
    pub fn release(&self, token: u64) -> Result<()> {
        // SAFETY: fd is valid
        unsafe {
            ioctl_release(self.fd(), token)?;
        }

        Ok(())
    }

    /// Kick the RX path for a specific queue
    ///
    /// Processes all pending descriptors in the selected queue's TX ring,
    /// injecting the associated packets into the kernel's RX path.
    ///
    /// # Returns
    /// The number of packets processed
    ///
    /// # Errors
    /// Returns an error if:
    /// - Device is not started
    /// - The ioctl fails
    pub fn kick_rx_queue(&self, queue_id: u32) -> Result<i32> {
        // SAFETY: fd is valid
        let processed = unsafe { ioctl_kick_rx_q(self.fd(), queue_id)? };
        Ok(processed)
    }

    /// Kick a batch of queues and return packets processed per queue
    ///
    /// The returned vector preserves the input queue order.
    ///
    /// # Errors
    /// Returns an error if any queue kick ioctl fails.
    pub fn kick_rx_queues(&self, queue_ids: &[u32]) -> Result<Vec<(u32, i32)>> {
        let mut processed = Vec::with_capacity(queue_ids.len());

        for &queue_id in queue_ids {
            processed.push((queue_id, self.kick_rx_queue(queue_id)?));
        }

        Ok(processed)
    }

    /// Clone a token to get a new token with a cloned SKB
    ///
    /// # Arguments
    /// * `token` - The token to clone
    ///
    /// # Returns
    /// The new token for the cloned SKB
    ///
    /// # Errors
    /// Returns an error if:
    /// - Token is not found (already released or invalid)
    /// - Memory allocation fails
    /// - The ioctl fails
    pub fn clone_token(&self, token: u64) -> Result<u64> {
        // SAFETY: fd is valid
        let new_token = unsafe { crate::sys::ioctl_clone(self.fd(), token)? };
        Ok(new_token)
    }

    /// Get the device ID
    /// If the device id is `x`, then the name of the device is rattan`x`
    ///
    /// # Returns
    /// The device ID
    ///
    /// # Errors
    /// Returns an error if the ioctl fails
    pub fn device_id(&self) -> Result<u32> {
        // SAFETY: fd is valid
        let device_id = unsafe { crate::sys::ioctl_device_id(self.fd()) }?;
        Ok(device_id)
    }

    /// Get the device name, which is `rattan` followed by the device ID
    ///
    /// # Returns
    /// The device name
    ///
    /// # Errors
    /// Returns an error if the ioctl fails
    pub fn device_name(&self) -> Result<String> {
        let device_id = self.device_id()?;
        Ok(format!("rattan{}", device_id))
    }
}

impl AsRawFd for RvnicDevice {
    fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}

impl Drop for RvnicDevice {
    fn drop(&mut self) {
        // Try to stop the device if it's running
        // Ignore errors - we're in drop
        if self.started {
            let _ = self.stop();
        }
        // UMEM and file are dropped automatically
        // The kernel will unpin pages and destroy the interface
    }
}
