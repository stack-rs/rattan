use std::collections::{BTreeMap, VecDeque};

use netem_trace::Delay;
use tokio::time::Instant;

use crate::cells::{bandwidth::LARGE_DURATION, Packet};

pub mod delay;

/// A priority queue to store packets and returns them after a certain delay
#[derive(Default, Debug, Clone, PartialEq, derive_more::Deref)]
pub struct DelayedQueue<P> {
    /// The BTreeMap stores packets ordered by the Instant they need to be sent
    #[deref]
    queue: BTreeMap<Instant, VecDeque<(P, Delay)>>,
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
    pub fn enqueue(&mut self, packet: P, delay: Delay) {
        let instant = packet.get_timestamp() + delay;
        self.queue
            .entry(instant)
            .or_default()
            .push_back((packet, delay));
    }

    /// Dequeues the packet that needs to be sent next
    pub fn dequeue(&mut self) -> Option<(Instant, P)> {
        if let Some(mut entry) = self.queue.first_entry() {
            if let Some((packet, _delay)) = entry.get_mut().pop_front() {
                let timestamp = *entry.key();
                if entry.get().is_empty() {
                    entry.remove_entry();
                }
                Some((timestamp, packet))
            } else {
                // The vec should always contains at least a value, so this should never happen.
                None
            }
        } else {
            None
        }
    }

    // /// Renqueues a packet to a specified instant.
    // ///
    // /// ⚠️ **Danger**: If there is already a packet at this instant, the previous packet is dropped
    // pub fn renqueue(&mut self, packet: P, instant: Instant) {
    //     self.queue.insert(instant, packet);
    // }

    /// Returns the next available instant for sending a packet.
    pub fn next_instant(&self) -> Instant {
        self.queue
            .first_key_value()
            .map(|(instant, _)| *instant)
            .unwrap_or(Instant::now() + LARGE_DURATION)
    }

    /// Returns a reference to the next packet to dequeue, if any
    pub fn next_packet(&self) -> Option<&P> {
        self.queue
            .first_key_value()
            .and_then(|(_instant, packets)| packets.get(0).map(|(packet, _delay)| packet))
    }
}
