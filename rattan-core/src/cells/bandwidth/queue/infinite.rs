use std::collections::VecDeque;
use tokio::time::Instant;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::PacketQueue;
use crate::cells::Packet;

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Default, Clone)]
pub struct InfiniteQueueConfig {}

impl InfiniteQueueConfig {
    pub fn new() -> Self {
        Self {}
    }
}

impl<P: Packet> From<InfiniteQueueConfig> for InfiniteQueue<P> {
    fn from(config: InfiniteQueueConfig) -> Self {
        InfiniteQueue::new(config).expect("InfiniteQueue::new should never fail")
    }
}

#[derive(Debug)]
pub struct InfiniteQueue<P> {
    queue: VecDeque<P>,
}

impl<P: Packet> Default for InfiniteQueue<P> {
    fn default() -> Self {
        Self::new(InfiniteQueueConfig::default()).expect("InfiniteQueue::new should never fail")
    }
}

impl<P> PacketQueue<P> for InfiniteQueue<P>
where
    P: Packet,
{
    type Config = InfiniteQueueConfig;

    fn new(_config: InfiniteQueueConfig) -> Result<Self, &'static str> {
        debug!("New InfiniteQueue");
        Ok(Self {
            queue: VecDeque::new(),
        })
    }

    fn configure(&mut self, _config: Self::Config) {}

    fn enqueue(&mut self, packet: P) {
        self.queue.push_back(packet);
    }

    fn dequeue_at(&mut self, _timestamp: Instant) -> Option<P> {
        self.queue.pop_front()
    }

    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    #[inline(always)]
    fn get_extra_length(&self) -> usize {
        0
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
