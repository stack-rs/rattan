use super::entry::{raw::RawLogEntry, tcp_ip_compact::TCPLogEntry};
use super::{LogEntry, LogEntryHeader};
use crate::log_entry::entry::raw::RawEntry;
use crate::log_entry::entry::ProtocolEntryVariant;
use binread::BinRead;
use bitfield::{BitRange, BitRangeMut};
use num_enum::TryFromPrimitive;
use plain::Plain;

#[derive(Debug, Clone, Copy, BinRead, Default)]
#[repr(C, packed(2))]
pub struct GeneralPktEntry {
    pub header: GeneralPktHeader,
    /// Timestamp in us
    pub ts: u32,
    pub pkt_length: u16,
}

unsafe impl Plain for GeneralPktEntry {}

#[derive(Debug, Clone, Copy, TryFromPrimitive)]
#[repr(u8)]
pub enum PktAction {
    Send = 0,
    Recv = 1,
    Drop = 2,
    Passthrough = 3,
}

#[derive(Debug, Clone, Copy, BinRead)]
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

#[derive(Debug, Clone, Copy, TryFromPrimitive, PartialEq, Eq)]
#[repr(u8)]
pub enum GeneralPacketType {
    Compact = 0,
    RawTCPIP = 1,
    RawIP = 2,
}

pub fn read_general_packet<R: std::io::Read + std::io::Seek>(
    header: LogEntryHeader,
    reader: &mut R,
    options: &binread::ReadOptions,
) -> binread::BinResult<LogEntry> {
    let pos = reader.stream_position()?;

    let general_pkt_entry = GeneralPktEntry::read_options(reader, options, ())?;

    match general_pkt_entry.header.get_type().try_into() {
        Ok(GeneralPacketType::Compact) => Ok(
            match ProtocolEntryVariant::read_options(reader, options, ())? {
                ProtocolEntryVariant::TCPIPCompact(tcp_entry) => {
                    LogEntry::CompactTCP(TCPLogEntry {
                        header,
                        general_pkt_entry,
                        tcp_entry,
                    })
                }
            },
        ),
        Ok(GeneralPacketType::RawTCPIP) | Ok(GeneralPacketType::RawIP) => {
            Ok(LogEntry::Raw(RawLogEntry {
                header,
                general_pkt_entry,
                raw_entry: RawEntry::read_options(reader, options, ())?,
            }))
        }
        _ => Err(binread::Error::NoVariantMatch { pos }),
    }
}
