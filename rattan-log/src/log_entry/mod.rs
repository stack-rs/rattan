use binread::{BinRead, Error};
use bitfield::{BitRange, BitRangeMut};
use num_enum::TryFromPrimitive;
use plain::Plain;
pub mod entry;
pub mod general_packet;
pub use general_packet::PktAction;

#[derive(Debug, Clone, Copy, BinRead, PartialEq, Eq)]
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
}

unsafe impl Plain for LogEntryHeader {}

impl Default for LogEntryHeader {
    fn default() -> Self {
        Self::new()
    }
}

use crate::log_entry::{
    entry::chunk_header::ChunkPrologue,
    entry::flow_entry::{read_flow_entry, TCPFlowEntry},
    entry::{raw::RawLogEntry, tcp_ip_compact::TCPLogEntry},
    general_packet::read_general_packet,
};
// All possible entries.
#[derive(Debug)]
pub enum LogEntry {
    CompactTCP(TCPLogEntry),
    Raw(RawLogEntry),
    Chunk(ChunkPrologue),
    TCPFlow(TCPFlowEntry),
}

#[derive(Debug, Clone, Copy, TryFromPrimitive)]
#[repr(u8)]
pub enum LogEntryType {
    GeneralPacket = 0,
    Cell = 1,
    FlowEntry = 2,
    ChunkPrologue = 15,
}

impl BinRead for LogEntry {
    type Args = ();
    fn read_options<R: std::io::Read + std::io::Seek>(
        reader: &mut R,
        options: &binread::ReadOptions,
        _args: Self::Args,
    ) -> binread::BinResult<Self> {
        let pos = reader.stream_position()?;

        let header = LogEntryHeader::read_options(reader, options, ())?;
        let log_entry_type = header.get_type();

        match log_entry_type.try_into() {
            // Packet log entry
            Ok(LogEntryType::GeneralPacket) => read_general_packet(header, reader, options),
            // Cell information
            Ok(LogEntryType::Cell) => {
                unimplemented!("Cell information to be added in the future")
            }
            // Flow entry
            Ok(LogEntryType::FlowEntry) => {
                read_flow_entry(reader, options).map(|flow| LogEntry::from((header, flow)))
            }
            Ok(LogEntryType::ChunkPrologue) => {
                ChunkPrologue::read_options(reader, options, (header,)).map(LogEntry::Chunk)
            }
            _ => Err(Error::NoVariantMatch { pos }),
        }
        .or_else(|e| {
            let end_location = pos + header.get_length() as u64;
            reader.seek(std::io::SeekFrom::Start(end_location))?;
            Err(e)
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::log_entry::general_packet::PktAction;
    use binread::BinReaderExt;
    use std::io::Cursor;

    #[test]
    pub fn parse_compact_tcp() {
        let data = hex::decode("200008019e3e01004a00160000000102a5bdcf8001020304a89100400f93020a")
            .unwrap();
        assert_eq!(data.len(), 32);
        let mut cursor = Cursor::new(data);

        if let LogEntry::CompactTCP(entry) = cursor.read_le::<LogEntry>().unwrap() {
            assert_eq!(entry.header.get_length(), 32);
            assert_eq!(entry.header.get_type(), 0);

            let pkt = entry.general_pkt_entry;
            assert_eq!(pkt.header.get_length(), 8);
            assert_eq!(pkt.header.get_pkt_action(), PktAction::Recv as u8);
            assert_eq!(pkt.pkt_length, 74);
            let ts = pkt.ts;
            assert_eq!(ts, 81566);

            let tcp = entry.tcp_entry;
            assert_eq!(tcp.header.get_length(), 22);
            assert_eq!(tcp.header.get_type(), 0);
            let (flow_id, seq, ack) = (tcp.flow_id, tcp.seq, tcp.ack);
            assert_eq!(flow_id, 33619968);
            assert_eq!(seq, 2161098149);
            assert_eq!(ack, 67305985);
            assert_eq!(tcp.ip_id, 37288);
            assert_eq!(tcp.ip_frag, 16384);
            assert_eq!(tcp.checksum, 37647);
            assert_eq!(tcp.flags, 2);
            assert_eq!(tcp.dataofs, 10);
        } else {
            unreachable!()
        }
    }

    #[test]
    pub fn parse_raw() {
        let data = hex::decode("100008139e3e01004a00061044332214").unwrap();
        assert_eq!(data.len(), 16);
        let mut cursor = Cursor::new(data);

        if let LogEntry::Raw(entry) = cursor.read_le::<LogEntry>().unwrap() {
            assert_eq!(entry.header.get_length(), 16);
            assert_eq!(entry.header.get_type(), 0);

            let pkt = entry.general_pkt_entry;
            assert_eq!(pkt.header.get_length(), 8);
            assert_eq!(pkt.header.get_pkt_action(), PktAction::Passthrough as u8);
            assert_eq!(pkt.pkt_length, 74);
            let ts = pkt.ts;
            assert_eq!(ts, 81566);

            let raw = entry.raw_entry;
            assert_eq!(raw.flow_index, 0x1006);

            let pointer = raw.pointer;
            // assert_eq!(flow_id, 0x19260817);
            assert_eq!(pointer.get_length(), 20);
            assert_eq!(pointer.get_offset(), 0x223344);
        } else {
            unreachable!()
        }
    }
}
