#![cfg_attr(not(test), deny(clippy::unwrap_used))]

use plain::Plain;

pub mod blob;
pub mod log_entry;
mod logger;

pub use log_entry::entry::{flow_entry::TCPFlow, raw::RawLogEntry, tcp_ip_compact::TCPLogEntry};
pub use logger::{
    FlowDesc, LOGGING_TX, RattanLogOp, reader::convert_log_to_pcapng, writer::file_logging_thread,
};

pub trait PlainBytes: Plain + Sized {
    fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        }
    }
}
impl<T: Plain + Sized> PlainBytes for T {}
