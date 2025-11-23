pub mod chunk_header;
pub mod flow_entry;
pub mod raw;
pub mod tcp_ip_compact;

use super::{LogEntry, LogEntryHeader};

use binread::BinRead;
use num_enum::TryFromPrimitive;
use tcp_ip_compact::TCPProtocolEntry;

pub type ProtocolHeader = LogEntryHeader;

#[derive(Debug, Clone, Copy, TryFromPrimitive, PartialEq, Eq)]
#[repr(u8)]
pub enum Protocol {
    TCPIPCompact = 0,
}

#[derive(Debug)]
pub enum ProtocolEntryVariant {
    TCPIPCompact(TCPProtocolEntry),
}

impl BinRead for ProtocolEntryVariant {
    type Args = ();
    fn read_options<R: std::io::Read + std::io::Seek>(
        reader: &mut R,
        options: &binread::ReadOptions,
        _args: Self::Args,
    ) -> binread::BinResult<Self> {
        let pos = reader.stream_position()?;
        let protocol_header = ProtocolHeader::read_options(reader, options, ())?;

        let result = match Protocol::try_from(protocol_header.get_type()).ok() {
            Some(Protocol::TCPIPCompact) => ProtocolEntryVariant::TCPIPCompact(
                TCPProtocolEntry::read_options(reader, options, (protocol_header,))?,
            ),

            _ => Err(binread::Error::NoVariantMatch { pos })?,
        };

        Ok(result)
    }
}

#[cfg(test)]
mod test {
    use crate::{log_entry::entry::Protocol, PlainBytes};
    use binread::BinRead;
    use std::io::Cursor;

    use crate::log_entry::entry::{
        tcp_ip_compact::TCPProtocolEntry, ProtocolEntryVariant, ProtocolHeader,
    };

    #[test]
    pub fn parse_compact_entry() {
        let mut compact_entry = TCPProtocolEntry {
            header: ProtocolHeader::default(),
            flow_id: 0x04030201,
            seq: 0x14131211,
            ack: 0x24232221,
            ip_id: 0x3231,
            ip_frag: 0x4241,
            checksum: 0x5251,
            flags: 0x61,
            dataofs: 0x71,
        };
        const ENTRY_SIZE: usize = 22;
        assert_eq!(ENTRY_SIZE, size_of::<TCPProtocolEntry>());
        compact_entry.header.set_length(ENTRY_SIZE as u16);
        compact_entry.header.set_type(Protocol::TCPIPCompact as u8);

        let bytes = compact_entry.as_bytes().to_owned();
        let mut cursor = Cursor::new(bytes);
        let decoded = ProtocolEntryVariant::read(&mut cursor).unwrap();

        match decoded {
            ProtocolEntryVariant::TCPIPCompact(entry) => assert_eq!(entry, compact_entry),
        }
    }
}
