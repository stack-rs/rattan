pub mod tcp_ip_compact;

pub mod raw;
use binread::BinRead;
use num_enum::TryFromPrimitive;

use crate::log_entry::{
    protocol::{raw::RawEntry, tcp_ip_compact::TCPProtocolEntry},
    LogEntryHeader,
};

pub type ProtocolHeader = LogEntryHeader;

#[derive(Debug, Clone, Copy, TryFromPrimitive, PartialEq, Eq)]
#[repr(u8)]
pub enum Protocol {
    TCPIPCompact = 0,
    TCPRaw = 1,
    TCPIPRaw = 2,
}

#[derive(Debug)]
pub enum ProtocolEntryVariant {
    TCPIPCompact(TCPProtocolEntry),
    Raw(RawEntry),
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
            Some(Protocol::TCPIPRaw) | Some(Protocol::TCPRaw) => ProtocolEntryVariant::Raw(
                RawEntry::read_options(reader, options, (protocol_header,))?,
            ),
            _ => Err(binread::Error::NoVariantMatch { pos })?,
        };

        Ok(result)
    }
}

#[cfg(test)]
mod test {
    use crate::{
        blob::RelativePointer,
        log_entry::protocol::{raw::RawEntry, Protocol},
        PlainBytes,
    };
    use binread::BinRead;
    use std::io::Cursor;

    use crate::log_entry::protocol::{
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
            _ => unreachable!(),
        }
    }

    #[test]
    pub fn parse_raw_entry() {
        let mut pointer = RelativePointer::new();
        pointer.set_length(20);
        pointer.set_offset(0x223344); // 24-bit

        let mut raw_entry = RawEntry {
            header: ProtocolHeader::default(),
            // flow_id: 0x19260817,
            pointer,
        };
        // const ENTRY_SIZE: usize = 10;
        const ENTRY_SIZE: usize = 6;
        assert_eq!(ENTRY_SIZE, size_of::<RawEntry>());
        raw_entry.header.set_length(ENTRY_SIZE as u16);
        raw_entry.header.set_type(Protocol::TCPRaw as u8);

        let bytes = raw_entry.as_bytes().to_owned();
        dbg!(hex::encode_upper(&bytes));
        let mut cursor = Cursor::new(bytes);
        let decoded = ProtocolEntryVariant::read(&mut cursor).unwrap();

        match decoded {
            ProtocolEntryVariant::Raw(entry) => assert_eq!(entry, raw_entry),
            _ => unreachable!(),
        }
    }
}
