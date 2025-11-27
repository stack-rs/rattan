use plain::Plain;

pub mod blob;
pub mod log_entry;
mod logger;

pub use logger::{file_writer::file_logging_thread, FlowDesc, RattanLogOp, LOGGING_TX};

pub use log_entry::entry::{raw::RawLogEntry, tcp_ip_compact::TCPLogEntry};

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

use clap::{command, Args};
use std::path::PathBuf;

/// Convert Rattan Packet Log file to pcapng file for each end
#[derive(Args, Debug, Default, Clone)]
#[command(rename_all = "kebab-case")]
pub struct LogConverterArgs {
    /// Input Rattan Packet Log file path
    pub input: PathBuf,
    /// Output pcapng file name prefix. take the input as default value
    pub output: Option<PathBuf>,
}

pub use logger::file_reader::convert_log_to_pcapng;
