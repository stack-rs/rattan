use bitfield::{BitRange, BitRangeMut};
use plain::Plain;
use std::io::Write;  // 

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use byteorder::{BigEndian, WriteBytesExt};
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
        
        // Write header bytes
        buf.extend_from_slice(&self.header.as_bytes());
        
        // Write general packet entry bytes
        buf.extend_from_slice(&self.general_pkt_entry.as_bytes());
        
        // Write TCP entry bytes
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
        let mut writer = &mut buf[..];
        writer.write_all(&self.header.as_bytes()).unwrap();
        writer.write_u32::<BigEndian>(self.ts).unwrap();
        writer.write_u16::<BigEndian>(self.pkt_length).unwrap();
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
        let mut writer = &mut buf[..];
        writer.write_all(&self.header.as_bytes()).unwrap();
        writer.write_u32::<BigEndian>(self.flow_id).unwrap();
        writer.write_u32::<BigEndian>(self.seq).unwrap();
        writer.write_u32::<BigEndian>(self.ack).unwrap();
        writer.write_u16::<BigEndian>(self.ip_id).unwrap();
        writer.write_u16::<BigEndian>(self.ip_frag_offset).unwrap();
        writer.write_u16::<BigEndian>(self.checksum).unwrap();
        writer.write_u8(self.flags).unwrap();
        writer.write_u8(self.padding).unwrap();
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
        let mut buf = [0u8; 2];
        buf.as_mut().write_u16::<BigEndian>(self.0).unwrap();
        buf
    }
}

unsafe impl Plain for ProtocolHeader {}

impl Default for ProtocolHeader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::{BigEndian, ByteOrder};

    #[test]
    fn test_log_entry_header_serialization() {
        let mut header = LogEntryHeader::new();
        header.set_type(0x5);     // 0101
        header.set_length(0xABC);  // 1010 1011 1100
        
        let bytes = header.as_bytes();
        assert_eq!(bytes, [0xAB, 0xC5]);
        assert_eq!(bytes.len(), 2);
        
        let reconstructed = LogEntryHeader(BigEndian::read_u16(&bytes));
        assert_eq!(reconstructed.get_type(), 0x5);
        assert_eq!(reconstructed.get_length(), 0xABC);
    }

    #[test]
    fn test_general_pkt_entry_serialization() {
        let entry = GeneralPktEntry {
            header: GeneralPktHeader::new(),
            ts: 0x12345678,
            pkt_length: 0x9ABC,
        };
        
        let bytes = entry.as_bytes();
        assert_eq!(bytes.len(), 8); // header(2) + ts(4) + length(2)
        
        assert_eq!(&bytes[0..2], [0x00, 0x00]);
        
        assert_eq!(&bytes[2..6], [0x12, 0x34, 0x56, 0x78]);
        
        assert_eq!(&bytes[6..8], [0x9A, 0xBC]);
    }

    #[test]
    fn test_tcp_protocol_entry_serialization() {
        let entry = TCPProtocolEntry {
            header: ProtocolHeader::new(),
            flow_id: 0x11223344,
            seq: 0x55667788,
            ack: 0x99AABBCC,
            ip_id: 0xDDEE,
            ip_frag_offset: 0xFF11,
            checksum: 0x2233,
            flags: 0x44,
            padding: 0x55,
        };
        
        let bytes = entry.as_bytes();
        assert_eq!(bytes.len(), 22);
        
        let mut offset = 0;
        assert_eq!(&bytes[offset..offset+2], [0x00, 0x00]); // header
        offset += 2;
        assert_eq!(&bytes[offset..offset+4], [0x11, 0x22, 0x33, 0x44]); // flow_id
        offset += 4;
        assert_eq!(&bytes[offset..offset+4], [0x55, 0x66, 0x77, 0x88]); // seq
        offset += 4;
        assert_eq!(&bytes[offset..offset+4], [0x99, 0xAA, 0xBB, 0xCC]); // ack
        offset += 4;
        assert_eq!(&bytes[offset..offset+2], [0xDD, 0xEE]); // ip_id
        offset += 2;
        assert_eq!(&bytes[offset..offset+2], [0xFF, 0x11]); // frag_offset
        offset += 2;
        assert_eq!(&bytes[offset..offset+2], [0x22, 0x33]); // checksum
        offset += 2;
        assert_eq!(bytes[offset], 0x44); // flags
        offset += 1;
        assert_eq!(bytes[offset], 0x55); // padding
    }

    #[test]
    fn test_full_tcp_log_entry_serialization() {
        let mut entry = TCPLogEntry::new();
        entry.header.set_type(0x1);
        entry.general_pkt_entry.ts = 0x12345678;
        entry.tcp_entry.flow_id = 0x11223344;
        
        let bytes = entry.as_bytes();
        assert_eq!(bytes.len(), 32);
        assert_eq!(&bytes[0..2], entry.header.as_bytes());
        assert_eq!(&bytes[2..10], entry.general_pkt_entry.as_bytes());
        assert_eq!(&bytes[10..32], entry.tcp_entry.as_bytes());

        let deserialized = TCPLogEntry::from_bytes(&bytes);
        assert_eq!(deserialized.header.get_type(), 0x1);
        assert_eq!(deserialized.general_pkt_entry.ts, 0x12345678);
        assert_eq!(deserialized.tcp_entry.flow_id, 0x11223344);
    }

    #[test]
    fn test_cross_serialization() {
        let mut entry = TCPLogEntry {
            header: LogEntryHeader(0),
            general_pkt_entry: GeneralPktEntry {
                header: GeneralPktHeader(0),
                ts: 0xDEADBEEF,
                pkt_length: 0xCAFE,
            },
            tcp_entry: TCPProtocolEntry {
                header: ProtocolHeader(0),
                flow_id: 0x12345678,
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
        
        let bytes = entry.as_bytes();
        assert_eq!(&bytes[2..6], [0xDE, 0xAD, 0xBE, 0xEF]); // ts
        assert_eq!(&bytes[10..14], [0x12, 0x34, 0x56, 0x78]); // flow_id
    }
}