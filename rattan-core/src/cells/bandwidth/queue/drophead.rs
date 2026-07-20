use std::collections::VecDeque;
use tokio::time::Instant;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tracing::debug;

#[cfg(feature = "serde")]
use super::serde_default;
use super::{BwType, PacketQueue};
use crate::cells::Packet;

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Default, Clone)]
pub struct DropHeadQueueConfig {
    pub packet_limit: Option<usize>,
    pub byte_limit: Option<usize>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "serde_default")
    )]
    pub bw_type: BwType,
}

impl DropHeadQueueConfig {
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
        bw_type: BwType,
    ) -> Self {
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
            bw_type,
        }
    }
}

impl<P: Packet> From<DropHeadQueueConfig> for DropHeadQueue<P> {
    fn from(config: DropHeadQueueConfig) -> Self {
        DropHeadQueue::new(config).expect("DropHeadQueue::new should never fail")
    }
}

#[derive(Debug)]
pub struct DropHeadQueue<P> {
    queue: VecDeque<P>,
    bw_type: BwType,
    packet_limit: Option<usize>,
    byte_limit: Option<usize>,
    now_bytes: usize,
}

impl<P: Packet> Default for DropHeadQueue<P> {
    fn default() -> Self {
        Self::new(DropHeadQueueConfig::default()).expect("DropHeadQueue::new should never fail")
    }
}

impl<P> PacketQueue<P> for DropHeadQueue<P>
where
    P: Packet,
{
    type Config = DropHeadQueueConfig;

    fn new(config: DropHeadQueueConfig) -> Result<Self, &'static str> {
        let packet_limit = config.packet_limit;
        let byte_limit = config.byte_limit;
        debug!(?config, "New DropHeadQueue");
        Ok(Self {
            queue: VecDeque::new(),
            bw_type: config.bw_type,
            packet_limit,
            byte_limit,
            now_bytes: 0,
        })
    }

    fn configure(&mut self, config: Self::Config) {
        self.packet_limit = config.packet_limit;
        self.byte_limit = config.byte_limit;
        self.bw_type = config.bw_type;
    }

    fn is_zero_buffer(&self) -> bool {
        self.packet_limit.is_some_and(|limit| limit == 0)
            || self.byte_limit.is_some_and(|limit| limit == 0)
    }

    fn enqueue(&mut self, packet: P) {
        let timestamp = packet.get_timestamp();
        self.now_bytes += packet.l3_length() + self.bw_type.extra_length();
        self.queue.push_back(packet);
        while self
            .packet_limit
            .is_some_and(|limit| self.queue.len() > limit)
            || self.byte_limit.is_some_and(|limit| self.now_bytes > limit)
        {
            let _packet = self.dequeue_at(timestamp).unwrap();
            #[cfg(test)]
            tracing::trace!(
                after_queue_len = self.queue.len(),
                after_now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &_packet.as_slice()[0..std::cmp::min(56, _packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) when enqueuing another packet", _packet.l3_length(), self.bw_type.extra_length()
            );
        }
    }

    fn dequeue_at(&mut self, _timestamp: Instant) -> Option<P> {
        match self.queue.pop_front() {
            Some(packet) => {
                self.now_bytes -= packet.l3_length() + self.bw_type.extra_length();
                Some(packet)
            }
            None => None,
        }
    }

    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    #[inline(always)]
    fn get_extra_length(&self) -> usize {
        self.bw_type.extra_length()
    }

    fn get_front_size(&self) -> Option<usize> {
        self.queue
            .front()
            .map(|packet| self.get_packet_size(packet))
    }

    fn length(&self) -> usize {
        self.queue.len()
    }
}
