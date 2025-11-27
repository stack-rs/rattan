use crate::{FlowDesc, PlainBytes};

use super::{LogEntry, LogEntryHeader};
use binread::BinRead;
use plain::Plain;
use std::net::{IpAddr, Ipv4Addr};

// The detailed spec of TCP flow entry
//
// 0                   1                   2                   3
// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |       LH.length       | LH.ty.|       FLE.length    | FLE.ty. |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                           flow id                             |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                            src.ip                             |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                            dst.ip                             |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |          src.port             |          dst.port             |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                   base ts (lower 32bit)                       |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                   base ts (upper 32bit)                       |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                          RESERVED                             |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                                                               |
// |                                                               |
// |                                                               |
// |              Options on SYN/SYN_ACK (40Bytes)                 |
// |                                                               |
// |                                                               |
// |                                                               |
// |                                                               |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//

pub type FlowEntryHeader = LogEntryHeader;
#[derive(Debug, Clone, Copy)]

pub struct TCPFlowEntry {
    pub header: LogEntryHeader,
    pub tcp_flow: TCPFlow,
}

impl TCPFlowEntry {
    pub fn new(tcp_flow: TCPFlow) -> Self {
        let mut header = LogEntryHeader::new();
        header.set_length(32);
        header.set_type(2);
        Self { header, tcp_flow }
    }
}

unsafe impl Plain for TCPFlowEntry {}

static_assertions::assert_eq_size!(TCPFlowEntry, [u8; 72]);

#[derive(Debug, Clone, Copy, BinRead)]
#[br(import(header: LogEntryHeader))]
#[repr(C, packed(2))]
pub struct TCPFlow {
    #[br(calc = header)]
    pub entryheader: FlowEntryHeader,
    pub flow_id: u32,
    pub src_ip: u32,
    pub dst_ip: u32,
    pub src_port: u16,
    pub dst_port: u16,
    pub base_ts: i64,
    pub _reserved: u32,
    pub options: TCPOption,
}

unsafe impl Plain for TCPFlow {}
static_assertions::assert_eq_size!(TCPFlow, [u8; 70]);

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct TCPOption {
    pub options: [u8; 40],
}
// As the TCP header can be at most 15 * 4 = 60Bytes in length, the options can be at most 40Bytes long.
//
// Also, since a `0x00` is a valid tcp option, which means termination of the options, we can just spare
// 40 Bytes, write from start, and leave unused part to be filled with 0.
//
impl BinRead for TCPOption {
    type Args = ();
    fn read_options<R: std::io::Read + std::io::Seek>(
        reader: &mut R,
        _options: &binread::ReadOptions,
        _args: Self::Args,
    ) -> binread::BinResult<Self> {
        let mut options = [0u8; 40];
        reader.read_exact(&mut options)?;
        Ok(Self { options })
    }
}

impl From<Option<Vec<u8>>> for TCPOption {
    fn from(value: Option<Vec<u8>>) -> Self {
        let mut options = [0u8; 40];
        if let Some(value) = value {
            let len = value.len().min(options.len());
            options[..len].copy_from_slice(&value[..len]);
        }
        Self { options }
    }
}

#[derive(Debug, Clone)]
pub enum FlowEntryVariant {
    TCP(TCPFlow),
}

impl From<TCPFlow> for FlowEntryVariant {
    fn from(value: TCPFlow) -> Self {
        Self::TCP(value)
    }
}

impl FlowEntryVariant {
    pub fn build(self) -> Vec<u8> {
        match self {
            FlowEntryVariant::TCP(tcp_flow) => {
                let entry = TCPFlowEntry::new(tcp_flow);
                entry.as_bytes().to_vec()
            }
        }
    }

    pub fn get_ip(&self) -> Vec<IpAddr> {
        vec![self.get_src_ip(), self.get_dst_ip()]
    }
    pub fn get_src_ip(&self) -> IpAddr {
        match self {
            FlowEntryVariant::TCP(tcp_flow) => IpAddr::V4(Ipv4Addr::from_bits(tcp_flow.src_ip)),
        }
    }
    pub fn get_dst_ip(&self) -> IpAddr {
        match self {
            FlowEntryVariant::TCP(tcp_flow) => IpAddr::V4(Ipv4Addr::from_bits(tcp_flow.dst_ip)),
        }
    }
}

impl From<(u32, i64, FlowDesc)> for FlowEntryVariant {
    // (flow_id, base_ts, flow_desc)
    fn from(value: (u32, i64, FlowDesc)) -> Self {
        let (flow_id, base_ts, flow_desc) = value;
        let mut entryheader = FlowEntryHeader::default();
        entryheader.set_length(32);
        match flow_desc {
            FlowDesc::TCP(src_ip, dst_ip, src_port, dst_port, options) => {
                entryheader.set_type(0);
                let entry = TCPFlow {
                    entryheader,
                    flow_id,
                    src_ip: src_ip.to_bits(),
                    dst_ip: dst_ip.to_bits(),
                    src_port,
                    dst_port,
                    base_ts,
                    _reserved: 0,
                    options: options.into(),
                };
                Self::TCP(entry)
            }
        }
    }
}

impl FlowEntryVariant {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            FlowEntryVariant::TCP(tcp) => tcp.as_bytes(),
        }
    }
}

pub fn read_flow_entry<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    options: &binread::ReadOptions,
) -> binread::BinResult<FlowEntryVariant> {
    let pos = reader.stream_position()?;

    let flow_header = FlowEntryHeader::read_options(reader, options, ())?;

    match flow_header.get_type() {
        // TCP Flow
        0 => TCPFlow::read_options(reader, options, (flow_header,)).map(FlowEntryVariant::TCP),
        // More kinds of flow entries may be supported here.
        _ => Err(binread::Error::NoVariantMatch { pos }),
    }
}

impl From<(LogEntryHeader, FlowEntryVariant)> for LogEntry {
    fn from(value: (LogEntryHeader, FlowEntryVariant)) -> Self {
        let (header, payload) = value;

        match payload {
            FlowEntryVariant::TCP(tcp_flow) => Self::TCPFlow(TCPFlowEntry { header, tcp_flow }),
        }
    }
}
