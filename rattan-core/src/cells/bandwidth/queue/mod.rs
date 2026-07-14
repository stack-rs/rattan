use std::collections::VecDeque;
use std::fmt::Debug;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

// All queue implementations can use this import
use super::BwType;
use crate::cells::{Packet, LARGE_DURATION};

mod codel;
mod drophead;
mod droptail;
mod infinite;

pub use codel::*;
pub use drophead::*;
pub use droptail::*;
pub use infinite::*;

#[cfg(feature = "serde")]
fn serde_default<T: Default + PartialEq>(t: &T) -> bool {
    *t == Default::default()
}

pub enum PacketInboundTryReceiveError {
    Empty,
    Failed,
}

pub trait PacketInbound<P> {
    fn try_receive(&mut self) -> Result<P, PacketInboundTryReceiveError>;
}

impl<P> PacketInbound<P> for UnboundedReceiver<P> {
    fn try_receive(&mut self) -> Result<P, PacketInboundTryReceiveError> {
        self.try_recv().map_err(|e| match e {
            TryRecvError::Empty => PacketInboundTryReceiveError::Empty,
            TryRecvError::Disconnected => PacketInboundTryReceiveError::Failed,
        })
    }
}

pub struct AQM<Q, P>
where
    Q: PacketQueue<P>,
    P: Packet,
{
    inbound_buffer: VecDeque<P>,
    queue: Q,
    latest_enqueue_timestamp: Option<Instant>,
}

impl<Q, P> AQM<Q, P>
where
    Q: PacketQueue<P>,
    P: Packet,
{
    pub fn new(queue: Q) -> Self {
        Self {
            inbound_buffer: VecDeque::with_capacity(1024),
            queue,
            latest_enqueue_timestamp: None,
        }
    }

    pub fn configure(&mut self, config: Q::Config) {
        self.queue.configure(config);
    }

    /// If this returns true, the caller should try to enqueue more packets.
    pub fn need_more_packets(&self, next_available: Instant) -> bool {
        self.latest_enqueue_timestamp
            .is_none_or(|t| t <= next_available)
    }

    /// Enqueue a packet into the AQM.
    ///
    /// If the inner queue is zero-buffered, the packet is returned immediately.
    pub fn enqueue(&mut self, packet: P) -> Option<P> {
        self.latest_enqueue_timestamp = packet.get_timestamp().into();
        if self.queue.is_zero_buffer() {
            packet.into()
        } else {
            self.inbound_buffer.push_back(packet);
            None
        }
    }

    /// Dequeue a packet from the AQM based on the timestamp.
    /// The function tries to maintain the queue status at the given timestamp before dequeuing a packet,
    /// by enqueuing any packets that should have been enqueued by that timestamp.
    ///
    /// The caller ensures that:
    ///   1) This function is not called before the wall-clock time of `timestamp`.
    ///   2) The timestamp should be non-descending.
    //  FIXME: The non-descending property can not be assured under multipath scenario.
    pub fn dequeue_at(&mut self, timestamp: Instant) -> Option<P> {
        while let Some(head) = self.inbound_buffer.front() {
            if head.get_timestamp() <= timestamp {
                self.queue.enqueue(self.inbound_buffer.pop_front().unwrap());
            } else {
                break;
            }
        }
        self.queue.dequeue_at(timestamp)
    }

    pub fn next_call_time(&self) -> Instant {
        if let Some(head) = self.inbound_buffer.front() {
            return head.get_timestamp();
        }
        Instant::now() + LARGE_DURATION
    }
}

pub trait PacketQueue<P>: Send
where
    P: Packet,
{
    #[cfg(feature = "serde")]
    type Config: for<'a> Deserialize<'a> + Serialize + Send + Debug;
    #[cfg(not(feature = "serde"))]
    type Config: Send + Debug;

    fn configure(&mut self, config: Self::Config);

    fn enqueue(&mut self, packet: P);

    /// If the queue is empty, return `None`
    fn dequeue_at(&mut self, timestamp: Instant) -> Option<P>;

    fn is_empty(&self) -> bool;

    /// Returns if the buffer is zero-sized.
    fn is_zero_buffer(&self) -> bool {
        false
    }

    /// How this queue measures the size of a packet.
    /// Should return 0 if it measures the size of a packet based on its L3 size.
    /// Should return 14 if it measures that based on its L2 size (L3 length + 14 bytes L2 header).
    fn get_extra_length(&self) -> usize {
        0
    }

    /// How this queue measures the size of a packet;
    #[inline(always)]
    fn get_packet_size(&self, packet: &P) -> usize {
        packet.l3_length() + self.get_extra_length()
    }

    /// If the queue is empty, return `None`
    fn get_front_size(&self) -> Option<usize>;

    fn length(&self) -> usize;
}
