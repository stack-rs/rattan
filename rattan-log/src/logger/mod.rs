use std::hash::Hash;
use std::net::Ipv4Addr;

use once_cell::sync::OnceCell;
use tokio::sync::mpsc::UnboundedSender;

use crate::RawLogEntry;

pub static LOGGING_TX: OnceCell<UnboundedSender<RattanLogOp>> = OnceCell::new();

pub mod build_pcap;
pub mod file_reader;
pub mod file_writer;
pub(crate) mod mmap;

#[derive(Debug, Clone, Eq)]
pub enum FlowDesc {
    // src.ip dst.ip src.port dst.port option
    TCP(Ipv4Addr, Ipv4Addr, u16, u16, Option<Vec<u8>>),
}

impl Hash for FlowDesc {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            FlowDesc::TCP(src_ip, dst_ip, src_port, dst_port, _options) => {
                "tcp".hash(state);
                src_ip.hash(state);
                dst_ip.hash(state);
                src_port.hash(state);
                dst_port.hash(state);
                // Delibrately skip `_options`, as it can only be got from packets with
                // SYN / SYN_ACK packet, as we need to let the other packets (in which _options
                // is `None`) considered to be from the same TCP flow in the `Flowmap`.
            }
        }
    }
}

impl PartialEq for FlowDesc {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (FlowDesc::TCP(a0, b0, c0, d0, _), FlowDesc::TCP(a1, b1, c1, d1, _)) => {
                (a0, b0, c0, d0) == (a1, b1, c1, d1)
            }
        }
    }
}

#[derive(Clone)]
pub enum RattanLogOp {
    // An encoded entry, which can be directly written into log entry file.
    Entry(Vec<u8>),
    // A partical built raw log entry.
    // 3 parts: (flow_id, raw_entry, raw_header)
    // Raw header shall be written to the `.raw` file.
    // After that, the flow_id, and the (offset, len) in the `.raw` where the raw_header was written shall
    // be used to build the raw log entry.
    RawEntry(u32, RawLogEntry, Vec<u8>),
    // A flow.
    // 3 parts: (flow_id , base_time_ns, flow_desc)
    Flow(u32, i64, FlowDesc),
    // End of Log
    End,
}
