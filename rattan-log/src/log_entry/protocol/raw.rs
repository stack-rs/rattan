use binread::BinRead;
use plain::Plain;

use super::ProtocolHeader;
use crate::{
    blob::RelativePointer,
    log_entry::{general_packet::GeneralPktEntry, protocol::Protocol, LogEntryHeader},
};

#[derive(Debug, Clone, Copy, BinRead, PartialEq, Eq, Default)]
#[br(import(header: ProtocolHeader))]
#[repr(C, packed(2))]
pub struct RawEntry {
    #[br(calc = header)]
    pub header: ProtocolHeader,
    // pub flow_id: u32,
    pub pointer: RelativePointer,
}

unsafe impl Plain for RawEntry {}
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
// |           Relative Offset to Chunk            |   Record.len  |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct RawLogEntry {
    pub header: LogEntryHeader,
    pub general_pkt_entry: GeneralPktEntry,
    pub raw_entry: RawEntry,
}

unsafe impl Plain for RawLogEntry {}

impl RawLogEntry {
    pub fn new_tcpip() -> Self {
        let mut entry = Self::default();

        entry.header.set_length(16);
        entry.header.set_type(0);

        entry.general_pkt_entry.header.set_length(8);
        entry.general_pkt_entry.header.set_type(0);

        entry.raw_entry.header.set_length(6);
        entry.raw_entry.header.set_type(Protocol::TCPIPRaw as u8);

        entry
    }
    pub fn new_tcp() -> Self {
        let mut entry = Self::default();

        entry.header.set_length(16);
        entry.header.set_type(0);

        entry.general_pkt_entry.header.set_length(8);
        entry.general_pkt_entry.header.set_type(0);

        entry.raw_entry.header.set_length(6);
        entry.raw_entry.header.set_type(Protocol::TCPRaw as u8);

        entry
    }
}
