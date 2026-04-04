use std::{
    fmt::Debug,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};

use async_trait::async_trait;
use num_enum::TryFromPrimitive;
#[cfg(feature = "serde")]
use serde::Deserialize;
use tokio::time::Duration;
#[cfg(any(test, doc))]
use tokio::time::Instant;

use crate::error::Error;

pub mod bandwidth;
pub mod delay;
pub mod external;
pub mod loss;
pub mod per_packet;
pub mod router;
pub mod shadow;
pub mod spy;
pub mod timed_config;
pub mod token_bucket;

pub use timed_config::TimedConfig;

// For back compatibility
pub use rattan_env::common::Packet;
pub use rattan_env::env::standard::StdPacket;

pub const LARGE_DURATION: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

/// A wrapper around a packet structure to analyse a cell's behaviour.
///
/// It stores the packet's creation timestamp, allowing us to inspect how long the packet
/// has been created, in terms of logical timestamp.
///
/// This exists only for test code. Especially for the test of cells that may impose
/// a delay on a packet (e.g. DelayCell, BwCell). After a packet leaves such a cell,
/// we check both wall-clock time and logical timestamp to determine how long the packet
/// has been delayed in the cell.
#[cfg(any(test, doc))]
mod test_packet {
    use super::*;
    use etherparse::{Ethernet2Header, Ipv4Header};
    use rattan_log::FlowDesc;

    #[derive(Clone, Debug, derive_more::Deref, derive_more::DerefMut)]
    pub struct TestPacket<P> {
        init_timestamp: Instant,
        #[deref]
        #[deref_mut]
        pub packet: P,
    }

    #[cfg(any(test, doc))]
    impl<P: Packet> TestPacket<P> {
        pub fn with_timestamp(buf: &[u8], init_timestamp: Instant) -> Self {
            let mut inner = P::from_raw_buffer(buf);
            inner.delay_until(init_timestamp);
            Self {
                init_timestamp,
                packet: inner,
            }
        }
    }

    impl<P: Packet> Packet for TestPacket<P> {
        type PacketGenerator = P::PacketGenerator;

        fn empty(maximum: usize, generator: &Self::PacketGenerator) -> Self {
            let packet = P::empty(maximum, generator);
            TestPacket {
                init_timestamp: packet.get_timestamp(),
                packet,
            }
        }

        fn from_raw_buffer(buf: &[u8]) -> Self {
            let packet = P::from_raw_buffer(buf);
            Self {
                init_timestamp: packet.get_timestamp(),
                packet,
            }
        }

        fn length(&self) -> usize {
            self.packet.length()
        }

        fn l2_length(&self) -> usize {
            self.packet.l2_length()
        }

        fn l3_length(&self) -> usize {
            self.packet.l3_length()
        }

        fn as_slice(&self) -> &[u8] {
            self.packet.as_slice()
        }

        fn as_raw_buffer(&mut self) -> &mut [u8] {
            self.packet.as_raw_buffer()
        }

        fn ip_hdr(&self) -> Option<Ipv4Header> {
            self.packet.ip_hdr()
        }

        fn ether_hdr(&self) -> Option<Ethernet2Header> {
            self.packet.ether_hdr()
        }

        fn get_timestamp(&self) -> Instant {
            self.packet.get_timestamp()
        }

        fn delay_by(&mut self, delay: Duration) {
            self.packet.delay_by(delay)
        }

        fn delay_until(&mut self, timestamp: Instant) {
            self.packet.delay_until(timestamp)
        }

        fn desc(&self) -> String {
            self.packet.desc()
        }

        fn flow_desc(&self) -> Option<FlowDesc> {
            self.packet.flow_desc()
        }

        fn set_flow_id(&mut self, flow_id: u32) {
            self.packet.set_flow_id(flow_id);
        }

        fn get_flow_id(&self) -> u32 {
            self.packet.get_flow_id()
        }
    }

    /// How long ago this packet was created, in terms of logical timestamp.
    impl<P: Packet> TestPacket<P> {
        pub fn delay(&self) -> Duration {
            self.get_timestamp() - self.init_timestamp
        }
    }
}

#[cfg(any(test, doc))]
pub use test_packet::TestPacket;

pub trait Ingress<P>: Send + Sync
where
    P: Packet,
{
    fn enqueue(&self, packet: P) -> Result<(), Error>;

    fn reset(&mut self) {}
}

#[async_trait]
pub trait Egress<P>: Send
where
    P: Packet,
{
    async fn dequeue(&mut self) -> Option<P>;

    fn reset(&mut self) {}

    fn change_state(&self, _state: CellState) {}

    /// Set the notify receiver for the cell to handle Start signals internally
    fn set_notify_receiver(
        &mut self,
        _notify_rx: tokio::sync::broadcast::Receiver<crate::control::RattanNotify>,
    ) {
    }
}

pub trait ControlInterface: Send + Sync + 'static {
    #[cfg(feature = "serde")]
    type Config: for<'a> Deserialize<'a> + Send;
    #[cfg(not(feature = "serde"))]
    type Config: Send;
    fn set_config(&self, config: Self::Config) -> Result<(), Error>;
    // TODO: add `set_config_at` to explicitly express the logical timestamp of config change.
}

#[cfg(feature = "serde")]
pub trait JsonControlInterface: Send + Sync {
    fn config_cell(&self, payload: serde_json::Value) -> Result<(), Error>;
}

#[cfg(feature = "serde")]
impl<T> JsonControlInterface for T
where
    T: ControlInterface,
{
    fn config_cell(&self, payload: serde_json::Value) -> Result<(), Error> {
        match serde_json::from_value(payload) {
            Ok(payload) => self.set_config(payload),
            Err(e) => Err(Error::ConfigError(e.to_string())),
        }
    }
}

#[async_trait]
pub trait Cell<P>
where
    P: Packet,
{
    type IngressType: Ingress<P> + 'static;
    type EgressType: Egress<P> + 'static;
    type ControlInterfaceType: ControlInterface;

    fn sender(&self) -> Arc<Self::IngressType>;
    fn receiver(&mut self) -> &mut Self::EgressType;
    fn into_receiver(self) -> Self::EgressType;
    fn control_interface(&self) -> Arc<Self::ControlInterfaceType>;
}

/// Called at the start of the cell's dequeue() method. Make sure that no packets shall be dequeued
/// until an expected Notification has been received ONCE.
#[macro_export]
macro_rules! wait_until_started {
    ($self:ident, $variant:ident) => {
        while !$self.started {
            if let Some(notify_rx) = &mut $self.notify_rx {
                match notify_rx.recv().await {
                    Ok($crate::control::RattanNotify::$variant) => {
                        $self.reset();
                        $self.change_state($crate::cells::CellState::Normal);
                        $self.started = true;
                    }
                    Ok(_) => {
                        // Ignore unexpected notifications.
                        continue;
                    }
                    Err(_) => {
                        // This happens when the notifier is dropped.
                        return None;
                    }
                }
            } else {
                // The notifier is not set unless the normal startup of Rattan has taken place. In some
                // non-integrated environments, the notifier may not be set, like unit tests for cells.
                break;
            }
        }
    };
}

/// Cells that replay a trace should refer to TRACE_START_INSTANT as the logical start instant of the trace.
#[cfg(not(feature = "first-packet"))]
pub use crate::core::CALIBRATED_START_INSTANT as TRACE_START_INSTANT;
#[cfg(feature = "first-packet")]
pub use crate::core::FIRST_PACKET_INSTANT as TRACE_START_INSTANT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum CellState {
    /// Drops all packets
    Drop = 0,
    /// Passes through all packets
    PassThrough = 1,
    /// Normal operation
    Normal = 2,
}

#[repr(transparent)]
pub struct AtomicCellState(AtomicU8);

impl AtomicCellState {
    pub const fn new(state: CellState) -> Self {
        Self(AtomicU8::new(state as u8))
    }

    #[inline]
    pub fn load(&self, order: Ordering) -> CellState {
        CellState::try_from_primitive(self.0.load(order)).expect("invalid CellState value")
    }

    #[inline]
    pub fn store(&self, state: CellState, order: Ordering) {
        self.0.store(state as u8, order);
    }
}

/// Evaluates cell state logic for incoming packets.
///
/// Automatically handles `Drop` (returning `None`) and `PassThrough` (returning `Some`).
/// For `Normal` states, it yields the packet to be used in the next logic step.
#[macro_export]
macro_rules! check_cell_state {
    ($state:expr, $packet:expr) => {
        match $state.load(std::sync::atomic::Ordering::Acquire) {
            $crate::cells::CellState::Drop => return None,
            $crate::cells::CellState::PassThrough => return Some($packet),
            $crate::cells::CellState::Normal => $packet,
        }
    };
}

// For test code only. Convert an Instant (machine time) to a relative time since the
// logical start point of trace start. This makes the test output more human-readable.
#[cfg(test)]
pub fn relative_time(time: Instant) -> Duration {
    let start = crate::cells::TRACE_START_INSTANT.get_or_init(Instant::now);
    time.duration_since(*start)
}
