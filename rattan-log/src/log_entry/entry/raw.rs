use binread::BinRead;
use plain::Plain;

use crate::{
    blob::RelativePointer,
    log_entry::{
        general_packet::{GeneralPacketType, GeneralPktEntry},
        LogEntryHeader,
    },
};

#[derive(Debug, Clone, Copy, BinRead, PartialEq, Eq, Default)]
#[repr(C, packed(2))]
pub struct RawEntry {
    pub flow_index: u16,
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
// |           GP.length           |          flow index           |
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
    pub fn new(pkt_type: GeneralPacketType) -> Self {
        let mut entry = Self::default();

        entry.header.set_length(16);
        entry.header.set_type(0);

        entry.general_pkt_entry.header.set_length(8);
        entry.general_pkt_entry.header.set_type(pkt_type as u8);

        entry
    }
}
