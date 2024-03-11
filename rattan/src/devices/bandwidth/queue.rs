use crate::devices::Packet;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt::Debug;
use tokio::time::{Duration, Instant};
use tracing::{debug, trace};

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

    // If the queue is empty, return `None`
    fn dequeue(&mut self) -> Option<P>;
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug)]
pub struct InfiniteQueueConfig {}

impl InfiniteQueueConfig {
    pub fn new() -> Self {
        Self {}
    }
}

impl<P> Into<InfiniteQueue<P>> for InfiniteQueueConfig {
    fn into(self) -> InfiniteQueue<P> {
        InfiniteQueue::new()
    }
}

#[derive(Debug)]
pub struct InfiniteQueue<P> {
    queue: VecDeque<P>,
}

impl<P> InfiniteQueue<P> {
    pub fn new() -> Self {
        debug!("New InfiniteQueue");
        Self {
            queue: VecDeque::new(),
        }
    }
}

impl<P> Default for InfiniteQueue<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P> PacketQueue<P> for InfiniteQueue<P>
where
    P: Packet,
{
    type Config = ();

    fn configure(&mut self, _config: Self::Config) {}

    fn enqueue(&mut self, packet: P) {
        self.queue.push_back(packet);
    }

    fn dequeue(&mut self) -> Option<P> {
        self.queue.pop_front()
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug)]
pub struct DropTailQueueConfig {
    pub packet_limit: Option<usize>,
    pub byte_limit: Option<usize>,
}

impl DropTailQueueConfig {
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
    ) -> Self {
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
        }
    }
}

impl<P> Into<DropTailQueue<P>> for DropTailQueueConfig {
    fn into(self) -> DropTailQueue<P> {
        DropTailQueue::new(self.packet_limit, self.byte_limit)
    }
}

#[derive(Debug)]
pub struct DropTailQueue<P> {
    queue: VecDeque<P>,
    packet_limit: Option<usize>,
    byte_limit: Option<usize>,
    now_bytes: usize,
}

impl<P> DropTailQueue<P> {
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
    ) -> Self {
        let packet_limit = packet_limit.into();
        let byte_limit = byte_limit.into();
        debug!(packet_limit, byte_limit, "New DropTailQueue");
        Self {
            queue: VecDeque::new(),
            packet_limit: packet_limit,
            byte_limit: byte_limit,
            now_bytes: 0,
        }
    }
}

impl<P> PacketQueue<P> for DropTailQueue<P>
where
    P: Packet,
{
    type Config = DropTailQueueConfig;

    fn configure(&mut self, config: Self::Config) {
        self.packet_limit = config.packet_limit;
        self.byte_limit = config.byte_limit;
    }

    fn enqueue(&mut self, packet: P) {
        if self
            .packet_limit
            .map_or(true, |limit| self.queue.len() < limit)
            && self
                .byte_limit
                .map_or(true, |limit| self.now_bytes + packet.length() <= limit)
        {
            self.now_bytes += packet.length();
            self.queue.push_back(packet);
        } else {
            trace!(
                queue_len = self.queue.len(),
                now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(len: {}) when enqueue", packet.length()
            );
        }
    }

    fn dequeue(&mut self) -> Option<P> {
        match self.queue.pop_front() {
            Some(packet) => {
                self.now_bytes -= packet.length();
                Some(packet)
            }
            None => None,
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug)]
pub struct DropHeadQueueConfig {
    pub packet_limit: Option<usize>,
    pub byte_limit: Option<usize>,
}

impl DropHeadQueueConfig {
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
    ) -> Self {
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
        }
    }
}

impl<P> Into<DropHeadQueue<P>> for DropHeadQueueConfig {
    fn into(self) -> DropHeadQueue<P> {
        DropHeadQueue::new(self.packet_limit, self.byte_limit)
    }
}

#[derive(Debug)]
pub struct DropHeadQueue<P> {
    queue: VecDeque<P>,
    packet_limit: Option<usize>,
    byte_limit: Option<usize>,
    now_bytes: usize,
}

impl<P> DropHeadQueue<P> {
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
    ) -> Self {
        let packet_limit = packet_limit.into();
        let byte_limit = byte_limit.into();
        debug!(packet_limit, byte_limit, "New DropHeadQueue");
        Self {
            queue: VecDeque::new(),
            packet_limit,
            byte_limit,
            now_bytes: 0,
        }
    }
}

impl<P> PacketQueue<P> for DropHeadQueue<P>
where
    P: Packet,
{
    type Config = DropHeadQueueConfig;

    fn configure(&mut self, config: Self::Config) {
        self.packet_limit = config.packet_limit;
        self.byte_limit = config.byte_limit;
    }

    fn enqueue(&mut self, packet: P) {
        self.now_bytes += packet.length();
        self.queue.push_back(packet);
        while self
            .packet_limit
            .map_or(false, |limit| self.queue.len() > limit)
            || self
                .byte_limit
                .map_or(false, |limit| self.now_bytes > limit)
        {
            let packet = self.dequeue().unwrap();
            trace!(
                after_queue_len = self.queue.len(),
                after_now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(len: {}) when enqueue another packet", packet.length()
            )
        }
    }

    fn dequeue(&mut self) -> Option<P> {
        match self.queue.pop_front() {
            Some(packet) => {
                self.now_bytes -= packet.length();
                Some(packet)
            }
            None => None,
        }
    }
}

// CoDel Queue Implementation Reference:
// https://github.com/torvalds/linux/blob/v6.6/include/net/codel.h
// https://github.com/ravinet/mahimahi/blob/0bd12164388bc109bbbd8ffa03a09e94adcbec5a/src/packet/codel_packet_queue.cc

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(default))]
#[derive(Debug)]
pub struct CoDelQueueConfig {
    pub packet_limit: Option<usize>, // the maximum number of packets in the queue, or None for unlimited
    pub byte_limit: Option<usize>, // the maximum number of bytes in the queue, or None for unlimited
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub interval: Duration, // width of moving time window
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub target: Duration, // target queue delay
    pub mtu: u32,                  // device MTU, or minimal queue backlog in bytes
}

impl Default for CoDelQueueConfig {
    fn default() -> Self {
        Self {
            packet_limit: None,
            byte_limit: None,
            interval: Duration::from_millis(100),
            target: Duration::from_millis(5),
            mtu: 1500,
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
    ) -> Self {
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
            interval,
            target,
            mtu,
        }
    }
}

impl<P> Into<CoDelQueue<P>> for CoDelQueueConfig {
    fn into(self) -> CoDelQueue<P> {
        CoDelQueue::new(self)
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
    pub fn new(config: CoDelQueueConfig) -> Self {
        debug!(?config, "New CoDelQueue");
        Self {
            queue: VecDeque::new(),
            config,
            now_bytes: 0,
            count: 0,
            lastcount: 0,
            dropping: false,
            first_above_time: None,
            drop_next: Instant::now(),
            ldelay: Duration::ZERO,
        }
    }
}

impl<P> CoDelQueue<P>
where
    P: Packet,
{
    fn should_drop(&mut self, packet: &P) -> bool {
        self.ldelay = Instant::now() - packet.get_timestamp();
        if self.ldelay < self.config.target || self.now_bytes <= self.config.mtu as usize {
            self.first_above_time = None;
            false
        } else {
            let mut ok_to_drop = false;
            match self.first_above_time {
                Some(first_above_time) => {
                    if Instant::now() >= first_above_time {
                        ok_to_drop = true;
                    }
                }
                None => {
                    self.first_above_time = Some(Instant::now() + self.config.interval);
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

    fn enqueue(&mut self, packet: P) {
        if self
            .config
            .packet_limit
            .map_or(true, |limit| self.queue.len() < limit)
            && self
                .config
                .byte_limit
                .map_or(true, |limit| self.now_bytes + packet.length() <= limit)
        {
            self.now_bytes += packet.length();
            self.queue.push_back(packet);
        } else {
            trace!(
                queue_len = self.queue.len(),
                now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(len: {}) when enqueue", packet.length()
            );
        }
    }

    fn dequeue(&mut self) -> Option<P> {
        match self.queue.pop_front() {
            Some(mut packet) => {
                self.now_bytes -= packet.length();
                let now = Instant::now();
                let drop = self.should_drop(&packet);
                trace!(
                    drop,
                    ldelay = ?self.ldelay,
                    count = self.count,
                    lastcount = self.lastcount,
                    dropping = self.dropping,
                    first_above_time_from_now = ?self.first_above_time.map(|t| t - Instant::now()),
                    drop_next_from_now = ?self.drop_next - Instant::now(),
                    after_queue_len = self.queue.len(),
                    after_now_bytes = self.now_bytes,
                    "dequeueing a new packet"
                );
                if self.dropping {
                    if !drop {
                        self.dropping = false;
                        trace!("Exit dropping state since packet should not drop");
                    } else {
                        while self.dropping && now >= self.drop_next {
                            self.count += 1;
                            trace!(
                                ldelay = ?self.ldelay,
                                count = self.count,
                                after_queue_len = self.queue.len(),
                                after_now_bytes = self.now_bytes,
                                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                                "Drop packet(len: {}) since should drop and now >= self.drop_next", packet.length()
                            );
                            let new_packet = self.queue.pop_front();
                            if new_packet.is_none() {
                                self.dropping = false;
                                trace!("Exit dropping state since queue is empty");
                                return None;
                            }
                            packet = new_packet.unwrap();
                            self.now_bytes -= packet.length();

                            if self.should_drop(&packet) {
                                self.drop_next = self.control_law(self.drop_next);
                                trace!(drop_next_from_now = ?self.drop_next - Instant::now());
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
                        "Drop packet(len: {}) as the first", packet.length()
                    );
                    let new_packet = self.queue.pop_front();
                    if new_packet.is_none() {
                        self.dropping = false;
                        trace!("Exit dropping state since queue is empty");
                        return None;
                    }
                    packet = new_packet.unwrap();
                    self.now_bytes -= packet.length();

                    self.dropping = true;
                    let delta = self.count - self.lastcount;
                    if delta > 1 && now - self.drop_next < 16 * self.config.interval {
                        self.count = delta;
                    } else {
                        self.count = 1;
                    }
                    self.lastcount = self.count;
                    self.drop_next = self.control_law(now);
                    trace!(
                        count = self.count,
                        delta,
                        drop_next_from_now = ?self.drop_next - Instant::now(),
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
}
