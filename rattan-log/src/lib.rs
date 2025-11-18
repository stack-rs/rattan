use plain::Plain;

pub mod blob;
pub mod log_entry;
mod logger;

pub use logger::{
    file_writer::{file_logging_thread, file_logging_thread_raw},
    FlowDesc, RattanLogOp, LOGGING_TX,
};

pub use log_entry::protocol::{raw::RawLogEntry, tcp_ip_compact::TCPLogEntry};

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
