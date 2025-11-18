use binread::BinRead;
use plain::Plain;

use super::ProtocolHeader;
use crate::log_entry::{general_packet::GeneralPktEntry, protocol::Protocol, LogEntryHeader};

#[derive(Debug, Clone, Copy, BinRead, PartialEq, Eq, Default)]
#[br(import(header: ProtocolHeader))]
#[repr(C, packed(2))]
pub struct TCPProtocolEntry {
    #[br(calc = header)]
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
#[derive(Debug, Clone, Copy, Default)]
#[repr(C, packed(2))]
pub struct TCPLogEntry {
    pub header: LogEntryHeader,
    pub general_pkt_entry: GeneralPktEntry,
    pub tcp_entry: TCPProtocolEntry,
}
unsafe impl Plain for TCPLogEntry {}
impl TCPLogEntry {
    pub fn new() -> Self {
        let mut entry = Self::default();

        entry.header.set_length(32);
        entry.header.set_type(0);

        entry.general_pkt_entry.header.set_length(8);
        entry.general_pkt_entry.header.set_type(0);

        entry.tcp_entry.header.set_length(22);
        entry
            .tcp_entry
            .header
            .set_type(Protocol::TCPIPCompact as u8);

        entry
    }
}
