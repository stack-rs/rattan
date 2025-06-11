use std::{collections::BTreeMap, time::Duration};

use tokio::time::Instant;

use crate::cells::Packet;

pub mod delay;

/// A priority queue to store packets and returns them after a certain delay
#[derive(Default)]
pub struct DelayedQueue<P> {
    /// The BTreeMap stores packets ordered by the Instant they need to be sent
    queue: BTreeMap<Instant, P>,
}

impl<P> DelayedQueue<P> {
    /// Creates a new DelayQueue instance.
    pub fn new() -> Self {
        Self {
            queue: BTreeMap::new(),
        }
    }
}

impl<P: Packet> DelayedQueue<P> {
    /// Enqueues a packet with a specified delay.
    ///
    /// The delay is computed since the timestamp of the packet
    ///
    /// If there is already a packet at the instant at which the packet should be sent,
    /// we add enough nano seconds to reach the next available instant
    pub fn enqueue(&mut self, packet: P, delay: Duration) {
        let mut instant = packet.get_timestamp() + delay;
        while self.queue.contains_key(&instant) {
            instant += Duration::from_nanos(1);
        }
        self.queue.insert(instant, packet);
    }

    /// Dequeues the packet that needs to be sent next
    pub fn dequeue(&mut self) -> Option<(Instant, P)> {
        self.queue.pop_first()
    }

    /// Renqueues a packet to a specified instant.
    ///
    /// ⚠️ **Danger**: If there is already a packet at this instant, the previous packet is dropped
    pub fn renqueue(&mut self, packet: P, instant: Instant) {
        self.queue.insert(instant, packet);
    }

    /// Returns the next available instant for sending a packet.
    pub fn next_instant(&self) -> Instant {
        self.queue
            .first_key_value()
            .map(|(instant, _)| *instant)
            .unwrap_or(Instant::now())
    }
}
