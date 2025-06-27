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
// |             ip.id             |         ip.frag_offset        |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |          ip.checksum          |   tcp.flags   |    padding    |
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
                ip_frag_offset: 0,
                checksum: 0,
                flags: 0,
                padding: 0,
            },
        };
        entry.header.set_length(32);
        entry.general_pkt_entry.header.set_length(8);
        entry.tcp_entry.header.set_length(22);
        entry
    }

    pub fn as_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(32);
        buf.extend_from_slice(&self.header.as_bytes());
        buf.extend_from_slice(&self.general_pkt_entry.as_bytes());
        buf.extend_from_slice(&self.tcp_entry.as_bytes());
        buf
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

    pub fn set_type(&mut self, value: u8) {
        self.0.set_bit_range(3, 0, value)
    }

    pub fn get_type(&self) -> u8 {
        self.0.bit_range(3, 0)
    }

    pub fn set_length(&mut self, value: u16) {
        self.0.set_bit_range(15, 4, value)
    }

    pub fn get_length(&self) -> u16 {
        self.0.bit_range(15, 4)
    }

    pub fn as_bytes(&self) -> [u8; 2] {
        self.0.to_be_bytes()
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

impl GeneralPktEntry {
    pub fn as_bytes(&self) -> [u8; 8] {
        let mut buf = [0u8; 8];
        buf[0..2].copy_from_slice(&self.header.as_bytes());
        buf[2..6].copy_from_slice(&self.ts.to_be_bytes());
        buf[6..8].copy_from_slice(&self.pkt_length.to_be_bytes());
        buf
    }
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

    pub fn set_type(&mut self, value: u8) {
        self.0.set_bit_range(3, 0, value)
    }

    pub fn get_type(&self) -> u8 {
        self.0.bit_range(3, 0)
    }

    pub fn set_pkt_action(&mut self, value: u8) {
        self.0.set_bit_range(7, 4, value)
    }

    pub fn get_pkt_action(&self) -> u8 {
        self.0.bit_range(7, 4)
    }

    pub fn set_length(&mut self, value: u16) {
        self.0.set_bit_range(15, 8, value)
    }

    pub fn get_length(&self) -> u16 {
        self.0.bit_range(15, 8)
    }

    pub fn as_bytes(&self) -> [u8; 2] {
        self.0.to_be_bytes()
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
    pub ip_frag_offset: u16,
    pub checksum: u16,
    pub flags: u8,
    pub padding: u8,
}

impl TCPProtocolEntry {
    pub fn as_bytes(&self) -> [u8; 22] {
        let mut buf = [0u8; 22];
        buf[0..2].copy_from_slice(&self.header.as_bytes());
        buf[2..6].copy_from_slice(&self.flow_id.to_be_bytes());
        buf[6..10].copy_from_slice(&self.seq.to_be_bytes());
        buf[10..14].copy_from_slice(&self.ack.to_be_bytes());
        buf[14..16].copy_from_slice(&self.ip_id.to_be_bytes());
        buf[16..18].copy_from_slice(&self.ip_frag_offset.to_be_bytes());
        buf[18..20].copy_from_slice(&self.checksum.to_be_bytes());
        buf[20] = self.flags;
        buf[21] = self.padding;
        buf
    }
}

unsafe impl Plain for TCPProtocolEntry {}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct ProtocolHeader(u16);

impl ProtocolHeader {
    pub fn new() -> Self {
        Self(0)
    }

    pub fn set_type(&mut self, value: u8) {
        self.0.set_bit_range(3, 0, value)
    }

    pub fn get_type(&self) -> u8 {
        self.0.bit_range(3, 0)
    }

    pub fn set_length(&mut self, value: u16) {
        self.0.set_bit_range(15, 4, value)
    }

    pub fn get_length(&self) -> u16 {
        self.0.bit_range(15, 4)
    }

    pub fn as_bytes(&self) -> [u8; 2] {
        self.0.to_be_bytes()
    }
}

unsafe impl Plain for ProtocolHeader {}

impl Default for ProtocolHeader {
    fn default() -> Self {
        Self::new()
    }
}