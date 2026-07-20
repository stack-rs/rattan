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
pub struct DropTailQueueConfig {
    pub packet_limit: Option<usize>, // None means unlimited
    pub byte_limit: Option<usize>,   // None means unlimited
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "serde_default")
    )]
    pub bw_type: BwType,
}

impl DropTailQueueConfig {
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

impl<P: Packet> From<DropTailQueueConfig> for DropTailQueue<P> {
    fn from(config: DropTailQueueConfig) -> Self {
        DropTailQueue::new(config).expect("DropTailQueue::new should never fail")
    }
}

#[derive(Debug)]
pub struct DropTailQueue<P> {
    queue: VecDeque<P>,
    bw_type: BwType,
    packet_limit: Option<usize>,
    byte_limit: Option<usize>,
    now_bytes: usize,
}

impl<P: Packet> Default for DropTailQueue<P> {
    fn default() -> Self {
        Self::new(DropTailQueueConfig::default()).expect("DropTailQueue::new should never fail")
    }
}

impl<P> PacketQueue<P> for DropTailQueue<P>
where
    P: Packet,
{
    type Config = DropTailQueueConfig;

    fn new(config: DropTailQueueConfig) -> Result<Self, &'static str> {
        let packet_limit = config.packet_limit;
        let byte_limit = config.byte_limit;
        debug!(?config, "New DropTailQueue");
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
        if self
            .packet_limit
            .is_none_or(|limit| self.queue.len() < limit)
            && self.byte_limit.is_none_or(|limit| {
                self.now_bytes + packet.l3_length() + self.bw_type.extra_length() <= limit
            })
        {
            self.now_bytes += packet.l3_length() + self.bw_type.extra_length();
            self.queue.push_back(packet);
        } else {
            #[cfg(test)]
            tracing::trace!(
                queue_len = self.queue.len(),
                now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) when enqueuing", packet.l3_length(), self.bw_type.extra_length()
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

impl<P> DropTailQueue<P>
where
    P: Packet,
{
    /// Retain only the packets for which `f` returns `true`, dropping the rest
    /// while keeping `now_bytes` consistent with the remaining packets.
    ///
    /// Used by the token bucket cell to drop packets that exceed the queue's
    /// `max_size` when it is shrunk (see `TokenBucketCellEgress::set_config`).
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&P) -> bool,
    {
        let extra_length = self.bw_type.extra_length();
        let now_bytes = &mut self.now_bytes;
        self.queue.retain(|packet| {
            if f(packet) {
                true
            } else {
                *now_bytes -= packet.l3_length() + extra_length;
                false
            }
        });
    }
}
