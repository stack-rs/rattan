//! Raw system types and ioctl definitions for rattan-vnic.
//! Must match `kernel/rattan_vnic_uapi.h`.

#![allow(missing_docs)]

use std::io;
use std::os::unix::io::RawFd;

// Configuration constants
pub const RATTAN_RING_SIZE: u32 = 262144;
pub const RATTAN_RING_MASK: u32 = RATTAN_RING_SIZE - 1;
pub const RATTAN_DEFAULT_CHUNK_SIZE: u32 = 256;
pub const RATTAN_MIN_CHUNK_SIZE: u32 = 256;
pub const RATTAN_MAX_CHUNK_SIZE: u32 = 65536;
pub const RATTAN_MAX_UMEM_SIZE: u64 = 128 << 30;
pub const RATTAN_HEADER_SIZE: u32 = 64;

// UMEM registration parameters
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanUmemReg {
    pub addr: u64,
    pub len: u64,
    pub chunk_size: u32,
    pub headroom: u32,
}

// Ring header - 128 bytes, cache-line aligned fields
#[repr(C)]
#[derive(Debug)]
pub struct RattanRing {
    pub producer: u32,
    _pad1: [u8; 60],
    pub consumer: u32,
    pub flags: u32,
    _pad2: [u32; 14],
}

pub const RATTAN_RING_NEED_WAKEUP: u32 = 1 << 0;

// Packet descriptor - 32 bytes
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanDesc {
    pub addr: u64,
    pub len: u32,
    pub options: u32,
    pub token: u64,
    pub timestamp: u64,
}

impl RattanDesc {
    /// Packet length of the underlying skb in kernel space
    pub fn get_skb_len(&self) -> u32 {
        self.options & 0xffff
    }
    
    /// Length of copied packet headers in UMEM
    pub fn get_umem_len(&self) -> u32 {
        self.len
    }
}

#[repr(C)]
pub struct RattanFillRing {
    pub ring: RattanRing,
    pub addrs: [u64; RATTAN_RING_SIZE as usize],
}

#[repr(C)]
pub struct RattanDropRing {
    pub ring: RattanRing,
    pub tokens: [u64; RATTAN_RING_SIZE as usize],
}

#[repr(C)]
pub struct RattanCompRing {
    pub ring: RattanRing,
    pub addrs: [u64; RATTAN_RING_SIZE as usize],
}

#[repr(C)]
pub struct RattanRxRing {
    pub ring: RattanRing,
    pub descs: [RattanDesc; RATTAN_RING_SIZE as usize],
}

#[repr(C)]
pub struct RattanTxRing {
    pub ring: RattanRing,
    pub descs: [RattanDesc; RATTAN_RING_SIZE as usize],
}

// Ring memory layout: RX | TX | FILL | COMP
pub const RATTAN_RINGS_SIZE: usize = std::mem::size_of::<RattanRxRing>()
    + std::mem::size_of::<RattanTxRing>()
    + std::mem::size_of::<RattanFillRing>()
    + std::mem::size_of::<RattanDropRing>()
    + std::mem::size_of::<RattanCompRing>();

pub const RATTAN_RX_RING_OFFSET: usize = 0;
pub const RATTAN_TX_RING_OFFSET: usize = std::mem::size_of::<RattanRxRing>();
pub const RATTAN_FILL_RING_OFFSET: usize =
    RATTAN_TX_RING_OFFSET + std::mem::size_of::<RattanTxRing>();
pub const RATTAN_COMP_RING_OFFSET: usize =
    RATTAN_FILL_RING_OFFSET + std::mem::size_of::<RattanFillRing>();
pub const RATTAN_DROP_RING_OFFSET: usize =
    RATTAN_COMP_RING_OFFSET + std::mem::size_of::<RattanCompRing>();

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanRingsReg {
    pub addr: u64,
    pub len: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanRingsRegQ {
    pub addr: u64,
    pub len: u64,
    pub queue_id: u32,
    pub _pad: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanShareUmemReq {
    pub source_fd: i32,
    pub _pad: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanPayloadReq {
    pub token: u64,
    pub buf: u64,
    pub len: u32,
    pub offset: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanPayloadResp {
    pub len: u32,
    pub total_len: u32,
}

/// Union for read_payload ioctl - kernel overwrites request with response
#[repr(C)]
pub union RattanPayloadIoctl {
    pub req: RattanPayloadReq,
    pub resp: RattanPayloadResp,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanReleaseReq {
    pub token: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanCloneReq {
    pub token: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanCloneResp {
    pub new_token: u64,
}

/// Start configuration
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RattanStartConfig {
    /// Target CPU for NAPI (-1 = use caller's CPU)
    pub napi_cpu: i32,
    pub _pad: u32,
}

// ioctl definitions
pub const RATTAN_VNIC_MAGIC: u8 = b'R';

const NR_REG_UMEM: u8 = 1;
const NR_REG_RINGS: u8 = 2;
const NR_SHARE_UMEM: u8 = 3;
const NR_START: u8 = 4;
const NR_STOP: u8 = 5;
const NR_REG_RINGS_Q: u8 = 6;
const NR_READ_PAYLOAD: u8 = 10;
const NR_RELEASE: u8 = 11;
const NR_KICK_RX: u8 = 12;
const NR_CLONE: u8 = 13;
const NR_KICK_RX_Q: u8 = 14;
const NR_DEVICE_ID: u8 = 15;

const fn iow<T>(magic: u8, nr: u8) -> libc::c_ulong {
    let size = std::mem::size_of::<T>() as libc::c_ulong;
    (1 << 30) | (size << 16) | ((magic as libc::c_ulong) << 8) | (nr as libc::c_ulong)
}

const fn ior<T>(magic: u8, nr: u8) -> libc::c_ulong {
    let size = std::mem::size_of::<T>() as libc::c_ulong;
    (2 << 30) | (size << 16) | ((magic as libc::c_ulong) << 8) | (nr as libc::c_ulong)
}

const fn iowr<T>(magic: u8, nr: u8) -> libc::c_ulong {
    let size = std::mem::size_of::<T>() as libc::c_ulong;
    (3 << 30) | (size << 16) | ((magic as libc::c_ulong) << 8) | (nr as libc::c_ulong)
}

const fn io(magic: u8, nr: u8) -> libc::c_ulong {
    ((magic as libc::c_ulong) << 8) | (nr as libc::c_ulong)
}

const IOCTL_REG_UMEM: libc::c_ulong = iow::<RattanUmemReg>(RATTAN_VNIC_MAGIC, NR_REG_UMEM);
const IOCTL_REG_RINGS: libc::c_ulong = iow::<RattanRingsReg>(RATTAN_VNIC_MAGIC, NR_REG_RINGS);
const IOCTL_SHARE_UMEM: libc::c_ulong = iow::<RattanShareUmemReq>(RATTAN_VNIC_MAGIC, NR_SHARE_UMEM);
const IOCTL_START: libc::c_ulong = iow::<RattanStartConfig>(RATTAN_VNIC_MAGIC, NR_START);
const IOCTL_STOP: libc::c_ulong = io(RATTAN_VNIC_MAGIC, NR_STOP);
const IOCTL_REG_RINGS_Q: libc::c_ulong = iow::<RattanRingsRegQ>(RATTAN_VNIC_MAGIC, NR_REG_RINGS_Q);
const IOCTL_READ_PAYLOAD: libc::c_ulong =
    iowr::<RattanPayloadReq>(RATTAN_VNIC_MAGIC, NR_READ_PAYLOAD);
const IOCTL_RELEASE: libc::c_ulong = iow::<RattanReleaseReq>(RATTAN_VNIC_MAGIC, NR_RELEASE);
const IOCTL_KICK_RX: libc::c_ulong = io(RATTAN_VNIC_MAGIC, NR_KICK_RX);
const IOCTL_CLONE: libc::c_ulong = iowr::<RattanCloneReq>(RATTAN_VNIC_MAGIC, NR_CLONE);
const IOCTL_KICK_RX_Q: libc::c_ulong = iow::<u32>(RATTAN_VNIC_MAGIC, NR_KICK_RX_Q);
const IOCTL_DEVICE_ID: libc::c_ulong = ior::<u32>(RATTAN_VNIC_MAGIC, NR_DEVICE_ID);

// Raw ioctl wrappers

/// # Safety
/// `fd` must be valid, memory region must remain valid until unregistered.
#[inline]
pub unsafe fn ioctl_reg_umem(fd: RawFd, reg: &RattanUmemReg) -> io::Result<()> {
    let ret = unsafe { libc::ioctl(fd, IOCTL_REG_UMEM, reg as *const RattanUmemReg) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// # Safety
/// `fd` must be valid, UMEM must be registered first.
#[inline]
pub unsafe fn ioctl_reg_rings(fd: RawFd, reg: &RattanRingsReg) -> io::Result<()> {
    let ret = unsafe { libc::ioctl(fd, IOCTL_REG_RINGS, reg as *const RattanRingsReg) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// # Safety
/// `fd` must be valid, UMEM must be registered first, and `queue_id` must be valid.
#[inline]
pub unsafe fn ioctl_reg_rings_q(fd: RawFd, reg: &RattanRingsRegQ) -> io::Result<()> {
    let ret = unsafe { libc::ioctl(fd, IOCTL_REG_RINGS_Q, reg as *const RattanRingsRegQ) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// # Safety
/// Both `fd` and `source_fd` must be valid rattan-vnic devices.
#[inline]
pub unsafe fn ioctl_share_umem(fd: RawFd, source_fd: RawFd) -> io::Result<()> {
    let req = RattanShareUmemReq { source_fd, _pad: 0 };
    let ret = unsafe { libc::ioctl(fd, IOCTL_SHARE_UMEM, &req as *const RattanShareUmemReq) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// # Safety
/// `fd` must be valid, UMEM and rings must be registered.
#[inline]
pub unsafe fn ioctl_start(fd: RawFd, napi_cpu: i32) -> io::Result<()> {
    let config = RattanStartConfig { napi_cpu, _pad: 0 };
    let ret = unsafe { libc::ioctl(fd, IOCTL_START, &config as *const RattanStartConfig) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// # Safety
/// `fd` must be valid.
#[inline]
pub unsafe fn ioctl_stop(fd: RawFd) -> io::Result<()> {
    let ret = unsafe { libc::ioctl(fd, IOCTL_STOP) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// # Safety
/// `fd` must be valid, `req.buf` must point to valid buffer of at least `req.len` bytes.
#[inline]
pub unsafe fn ioctl_read_payload(
    fd: RawFd,
    req: &RattanPayloadReq,
) -> io::Result<RattanPayloadResp> {
    let mut ioctl_data = RattanPayloadIoctl { req: *req };
    let ret = unsafe {
        libc::ioctl(
            fd,
            IOCTL_READ_PAYLOAD,
            &mut ioctl_data as *mut RattanPayloadIoctl,
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: After successful ioctl, kernel has written response into the union
    Ok(unsafe { ioctl_data.resp })
}

/// # Safety
/// `fd` must be valid, `token` must be valid.
#[inline]
pub unsafe fn ioctl_release(fd: RawFd, token: u64) -> io::Result<()> {
    let req = RattanReleaseReq { token };
    let ret = unsafe { libc::ioctl(fd, IOCTL_RELEASE, &req as *const RattanReleaseReq) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// # Safety
/// `fd` must be valid.
#[inline]
pub unsafe fn ioctl_kick_rx(fd: RawFd) -> io::Result<i32> {
    let ret = unsafe { libc::ioctl(fd, IOCTL_KICK_RX) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ret)
}

/// # Safety
/// `fd` must be valid and `queue_id` must be valid.
#[inline]
pub unsafe fn ioctl_kick_rx_q(fd: RawFd, queue_id: u32) -> io::Result<i32> {
    let mut qid = queue_id;
    let ret = unsafe { libc::ioctl(fd, IOCTL_KICK_RX_Q, &mut qid as *mut u32) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ret)
}

/// # Safety
/// `fd` must be valid, `token` must be valid.
#[inline]
pub unsafe fn ioctl_clone(fd: RawFd, token: u64) -> io::Result<u64> {
    let mut buf: u64 = token;
    let ret = unsafe { libc::ioctl(fd, IOCTL_CLONE, &mut buf as *mut u64) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

/// # Safety
/// `fd` must be valid.
#[inline]
pub unsafe fn ioctl_device_id(fd: RawFd) -> io::Result<u32> {
    let mut buf: u32 = 0;
    let ret = unsafe { libc::ioctl(fd, IOCTL_DEVICE_ID, &mut buf as *mut u32) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_struct_sizes() {
        assert_eq!(std::mem::size_of::<RattanUmemReg>(), 24);
        assert_eq!(std::mem::size_of::<RattanRingsReg>(), 16);
        assert_eq!(std::mem::size_of::<RattanRingsRegQ>(), 24);
        assert_eq!(std::mem::size_of::<RattanDesc>(), 32);
        assert_eq!(std::mem::size_of::<RattanRing>(), 128);
    }

    #[test]
    fn test_ring_offsets() {
        let ring: RattanRing = unsafe { std::mem::zeroed() };
        let base = &ring as *const _ as usize;
        assert_eq!(&ring.producer as *const _ as usize - base, 0);
        assert_eq!(&ring.consumer as *const _ as usize - base, 64);
        assert_eq!(&ring.flags as *const _ as usize - base, 68);
    }

    #[test]
    fn test_ioctl_codes() {
        assert_eq!(IOCTL_REG_UMEM, 0x40185201);
        assert_eq!(IOCTL_REG_RINGS, 0x40105202);
        assert_eq!(IOCTL_REG_RINGS_Q, 0x40185206);
        assert_eq!(IOCTL_START, 0x40085204);
        assert_eq!(IOCTL_STOP, 0x5205);
        assert_eq!(IOCTL_KICK_RX_Q, 0x4004520e);
        assert_eq!(IOCTL_DEVICE_ID, 0x8004520f)
    }
}
