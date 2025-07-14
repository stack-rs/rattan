use bitfield::{BitRange, BitRangeMut};
use plain::Plain;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::cells::FlowDesc;

pub enum RattanLogOp {
    Entry(Vec<u8>),
    Flow(u32, i64, FlowDesc),
    End,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone)]
pub struct FlowEntry {
    pub flow_id: u32,
    pub base_ts: i64,
    pub flow_desc: FlowDesc,
}

// The detailed spec of this log entry:
//
// 0                   1                   2                   3
// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |       LH.length       | LH.ty.|   GPH.length  |GPH.ac.|GPH.ty.|
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                          GP.timestamp                         |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |           GP.length           |       PRH.length      |PRH.ty.|
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                          tcp.flow_id                          |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                            tcp.seq                            |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                            tcp.ack                            |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |             ip.id             |            ip.frag            |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |          ip.checksum          |   tcp.flags   |  tcp.dataofs  |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
#[derive(Debug, Clone, Copy)]
#[repr(C, packed(2))]
pub struct TCPLogEntry {
    pub header: LogEntryHeader,
    pub general_pkt_entry: GeneralPktEntry,
    pub tcp_entry: TCPProtocolEntry,
}

impl TCPLogEntry {
    pub fn new() -> Self {
        let mut entry = Self {
            header: LogEntryHeader::new(),
            general_pkt_entry: GeneralPktEntry {
                header: GeneralPktHeader::new(),
                ts: 0,
                pkt_length: 0,
            },
            tcp_entry: TCPProtocolEntry {
                header: ProtocolHeader::new(),
                flow_id: 0,
                seq: 0,
                ack: 0,
                ip_id: 0,
                ip_frag: 0,
                checksum: 0,
                flags: 0,
                dataofs: 0,
            },
        };
        entry.header.set_length(32);
        entry.general_pkt_entry.header.set_length(8);
        entry.tcp_entry.header.set_length(22);
        entry
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        }
    }

    pub fn from_bytes(buf: &[u8]) -> &Self {
        plain::from_bytes(buf).expect("The buffer is either too short or not aligned!")
    }
}

unsafe impl Plain for TCPLogEntry {}

impl Default for TCPLogEntry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct LogEntryHeader(u16);

impl LogEntryHeader {
    pub fn new() -> Self {
        Self(0)
    }

    pub fn set_length(&mut self, value: u16) {
        self.0.set_bit_range(11, 0, value)
    }

    pub fn get_length(&self) -> u16 {
        self.0.bit_range(11, 0)
    }

    pub fn set_type(&mut self, value: u8) {
        self.0.set_bit_range(15, 12, value)
    }

    pub fn get_type(&self) -> u8 {
        self.0.bit_range(15, 12)
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        }
    }
}

unsafe impl Plain for LogEntryHeader {}

impl Default for LogEntryHeader {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed(2))]
pub struct GeneralPktEntry {
    pub header: GeneralPktHeader,
    /// Timestamp in us
    pub ts: u32,
    pub pkt_length: u16,
}

unsafe impl Plain for GeneralPktEntry {}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum PktAction {
    Send = 0,
    Recv = 1,
    Drop = 2,
    Passthrough = 3,
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct GeneralPktHeader(u16);

impl GeneralPktHeader {
    pub fn new() -> Self {
        Self(0)
    }

    pub fn set_length(&mut self, value: u16) {
        self.0.set_bit_range(7, 0, value)
    }

    pub fn get_length(&self) -> u16 {
        self.0.bit_range(7, 0)
    }

    pub fn set_pkt_action(&mut self, value: u8) {
        self.0.set_bit_range(11, 8, value)
    }

    pub fn get_pkt_action(&self) -> u8 {
        self.0.bit_range(11, 8)
    }

    pub fn set_type(&mut self, value: u8) {
        self.0.set_bit_range(15, 12, value)
    }

    pub fn get_type(&self) -> u8 {
        self.0.bit_range(15, 12)
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        }
    }
}

unsafe impl Plain for GeneralPktHeader {}

impl Default for GeneralPktHeader {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed(2))]
pub struct TCPProtocolEntry {
    pub header: ProtocolHeader,
    pub flow_id: u32,
    pub seq: u32,
    pub ack: u32,
    pub ip_id: u16,
    pub ip_frag: u16,
    pub checksum: u16,
    pub flags: u8,
    pub dataofs: u8,
}

unsafe impl Plain for TCPProtocolEntry {}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct ProtocolHeader(u16);

impl ProtocolHeader {
    pub fn new() -> Self {
        Self(0)
    }

    pub fn set_length(&mut self, value: u16) {
        self.0.set_bit_range(11, 0, value)
    }

    pub fn get_length(&self) -> u16 {
        self.0.bit_range(11, 0)
    }

    pub fn set_type(&mut self, value: u8) {
        self.0.set_bit_range(15, 12, value)
    }

    pub fn get_type(&self) -> u8 {
        self.0.bit_range(15, 12)
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self as *const ProtocolHeader as *const u8,
                std::mem::size_of::<ProtocolHeader>(),
            )
        }
    }
}

unsafe impl Plain for ProtocolHeader {}

impl Default for ProtocolHeader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_ser() {
        let mut entry = TCPLogEntry::new();
        entry.header.set_length(0x321);
        entry.header.set_type(0x4);
        entry.general_pkt_entry.header.set_length(0x65);
        entry.general_pkt_entry.header.set_pkt_action(0x7);
        entry.general_pkt_entry.header.set_type(0x8);
        entry.general_pkt_entry.ts = 0x8765_4321;
        entry.general_pkt_entry.pkt_length = 0x4321;
        entry.tcp_entry.header.set_length(0x765);
        entry.tcp_entry.header.set_type(0x8);
        entry.tcp_entry.flow_id = 0x8765_4321;
        entry.tcp_entry.seq = 0x8765_4321;
        entry.tcp_entry.ack = 0x8765_4321;
        entry.tcp_entry.ip_id = 0x4321;
        entry.tcp_entry.ip_frag = 0x8765;
        entry.tcp_entry.checksum = 0x4321;
        entry.tcp_entry.flags = 0x65;
        entry.tcp_entry.dataofs = 0x87;
        let bytes: &[u8] = entry.as_bytes();
        assert_eq!(bytes.len(), std::mem::size_of::<TCPLogEntry>());
        assert_eq!(bytes.len(), 32);
        for i in 0..8 {
            assert_eq!(bytes[4 * i], 0x21);
            assert_eq!(bytes[4 * i + 1], 0x43);
            assert_eq!(bytes[4 * i + 2], 0x65);
            assert_eq!(bytes[4 * i + 3], 0x87);
        }
    }
}
