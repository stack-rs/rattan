// CoDel Queue Implementation Reference:
// https://github.com/torvalds/linux/blob/v6.6/include/net/codel.h
// https://github.com/ravinet/mahimahi/blob/0bd12164388bc109bbbd8ffa03a09e94adcbec5a/src/packet/codel_packet_queue.cc

use std::collections::VecDeque;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, Instant};
use tracing::{debug, trace};

#[cfg(feature = "serde")]
use super::serde_default;
use super::{BwType, PacketQueue};
use crate::cells::Packet;

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(default))]
#[derive(Debug, Clone)]
pub struct CoDelQueueConfig {
    pub packet_limit: Option<usize>, // the maximum number of packets in the queue, or None for unlimited
    pub byte_limit: Option<usize>, // the maximum number of bytes in the queue, or None for unlimited
    #[cfg_attr(feature = "serde", serde(with = "crate::utils::serde::duration"))]
    pub interval: Duration, // width of moving time window
    #[cfg_attr(feature = "serde", serde(with = "crate::utils::serde::duration"))]
    pub target: Duration, // target queue delay
    pub mtu: u32,                  // cell MTU, or minimal queue backlog in bytes
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "serde_default")
    )]
    pub bw_type: BwType,
}

impl Default for CoDelQueueConfig {
    fn default() -> Self {
        Self {
            packet_limit: None,
            byte_limit: None,
            interval: Duration::from_millis(100),
            target: Duration::from_millis(5),
            mtu: 1500,
            bw_type: BwType::default(),
        }
    }
}

impl CoDelQueueConfig {
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
        interval: Duration,
        target: Duration,
        mtu: u32,
        bw_type: BwType,
    ) -> Self {
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
            interval,
            target,
            mtu,
            bw_type,
        }
    }
}

impl<P> From<CoDelQueueConfig> for CoDelQueue<P> {
    fn from(config: CoDelQueueConfig) -> Self {
        CoDelQueue::new(config).expect("CoDelQueue::new should never fail")
    }
}

#[derive(Debug)]
pub struct CoDelQueue<P> {
    queue: VecDeque<P>,
    config: CoDelQueueConfig,
    now_bytes: usize, // the current number of bytes in the queue

    count: u32, // how many drops we've done since the last time we entered dropping state
    lastcount: u32, // count at entry to dropping state
    dropping: bool, // set to true if in dropping state
    first_above_time: Option<Instant>, // when we went (or will go) continuously above target for interval
    drop_next: Instant,                // time to drop next packet, or when we dropped last
    ldelay: Duration,                  // sojourn time of last dequeued packet
}

impl<P> CoDelQueue<P> {
    pub fn new(config: CoDelQueueConfig) -> Result<Self, &'static str> {
        debug!(?config, "New CoDelQueue");
        Ok(Self {
            queue: VecDeque::new(),
            config,
            now_bytes: 0,
            count: 0,
            lastcount: 0,
            dropping: false,
            first_above_time: None,
            drop_next: Instant::now(),
            ldelay: Duration::ZERO,
        })
    }
}

impl<P> Default for CoDelQueue<P>
where
    P: Packet,
{
    fn default() -> Self {
        Self::new(CoDelQueueConfig::default()).expect("CoDelQueue::new should never fail")
    }
}

impl<P> CoDelQueue<P>
where
    P: Packet,
{
    fn should_drop(&mut self, packet: &P, now: Instant) -> bool {
        self.ldelay = now - packet.get_timestamp();
        if self.ldelay < self.config.target || self.now_bytes <= self.config.mtu as usize {
            self.first_above_time = None;
            false
        } else {
            let mut ok_to_drop = false;
            match self.first_above_time {
                Some(first_above_time) => {
                    if now >= first_above_time {
                        ok_to_drop = true;
                    }
                }
                None => {
                    self.first_above_time = Some(now + self.config.interval);
                }
            }
            ok_to_drop
        }
    }

    fn control_law(&self, t: Instant) -> Instant {
        t + self.config.interval.div_f64(f64::sqrt(self.count as f64))
    }
}

impl<P> PacketQueue<P> for CoDelQueue<P>
where
    P: Packet,
{
    type Config = CoDelQueueConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn is_zero_buffer(&self) -> bool {
        self.config.packet_limit.is_some_and(|limit| limit == 0)
            || self.config.byte_limit.is_some_and(|limit| limit == 0)
    }

    fn enqueue(&mut self, packet: P) {
        if self
            .config
            .packet_limit
            .is_none_or(|limit| self.queue.len() < limit)
            && self.config.byte_limit.is_none_or(|limit| {
                self.now_bytes + packet.l3_length() + self.config.bw_type.extra_length() <= limit
            })
        {
            self.now_bytes += packet.l3_length() + self.config.bw_type.extra_length();
            self.queue.push_back(packet);
        } else {
            #[cfg(test)]
            trace!(
                queue_len = self.queue.len(),
                now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) when enqueuing",
                packet.l3_length(),
                self.config.bw_type.extra_length()
            );
        }
    }

    fn dequeue_at(&mut self, timestamp: Instant) -> Option<P> {
        match self.queue.pop_front() {
            Some(mut packet) => {
                self.now_bytes -= packet.l3_length() + self.config.bw_type.extra_length();
                let drop = self.should_drop(&packet, timestamp);
                trace!(
                    drop,
                    ldelay = ?self.ldelay,
                    count = self.count,
                    lastcount = self.lastcount,
                    dropping = self.dropping,
                    first_above_time_from_now = ?self.first_above_time.map(|t| t - timestamp),
                    drop_next_from_now = ?self.drop_next - timestamp,
                    after_queue_len = self.queue.len(),
                    after_now_bytes = self.now_bytes,
                    "dequeueing a new packet"
                );
                if self.dropping {
                    if !drop {
                        self.dropping = false;
                        trace!("Exit dropping state since packet should not be dropped");
                    } else {
                        while self.dropping && timestamp >= self.drop_next {
                            self.count += 1;
                            trace!(
                                ldelay = ?self.ldelay,
                                count = self.count,
                                after_queue_len = self.queue.len(),
                                after_now_bytes = self.now_bytes,
                                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                                "Drop packet(l3_len: {}, extra_len: {}) since should drop and now >= self.drop_next",
                                packet.l3_length(),
                                self.config.bw_type.extra_length()
                            );
                            let new_packet = self.queue.pop_front();
                            packet = match new_packet {
                                Some(packet) => packet,
                                None => {
                                    self.dropping = false;
                                    trace!("Exit dropping state since queue is empty");
                                    return None;
                                }
                            };
                            self.now_bytes -=
                                packet.l3_length() + self.config.bw_type.extra_length();

                            if self.should_drop(&packet, timestamp) {
                                self.drop_next = self.control_law(self.drop_next);
                                trace!(drop_next_from_now = ?self.drop_next - timestamp);
                            } else {
                                self.dropping = false;
                                trace!("Exit dropping state since packet should not drop");
                            }
                        }
                    }
                } else if drop {
                    trace!(
                        ldelay = ?self.ldelay,
                        after_queue_len = self.queue.len(),
                        after_now_bytes = self.now_bytes,
                        header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                        "Drop packet(l3_len: {}, extra_len: {}) as the first",
                        packet.l3_length(),
                        self.config.bw_type.extra_length()
                    );
                    let new_packet = self.queue.pop_front();
                    let packet = match new_packet {
                        Some(packet) => packet,
                        None => {
                            self.dropping = false;
                            trace!("Exit dropping state since queue is empty");
                            return None;
                        }
                    };
                    self.now_bytes -= packet.l3_length() + self.config.bw_type.extra_length();

                    self.dropping = true;
                    let delta = self.count - self.lastcount;
                    if delta > 1 && timestamp - self.drop_next < 16 * self.config.interval {
                        self.count = delta;
                    } else {
                        self.count = 1;
                    }
                    self.lastcount = self.count;
                    self.drop_next = self.control_law(timestamp);
                    trace!(
                        count = self.count,
                        delta,
                        drop_next_from_now = ?self.drop_next - timestamp,
                        "Enter dropping state"
                    );
                }
                Some(packet)
            }
            None => {
                self.dropping = false;
                trace!("Exit dropping state since queue is empty");
                None
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    #[inline(always)]
    fn get_extra_length(&self) -> usize {
        self.config.bw_type.extra_length()
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
