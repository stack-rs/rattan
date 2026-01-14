use std::collections::VecDeque;
use std::fmt::Debug;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::{
    sync::mpsc::{error::TryRecvError, UnboundedReceiver},
    time::{Duration, Instant},
};
use tracing::{debug, trace};

use super::BwType;
use crate::cells::Packet;

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
    /// The function tries to maintain the queue status at the given timestamp before dequeing a packet,
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
        self.queue.dequeue()
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
    fn dequeue(&mut self) -> Option<P>;

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

    fn retain<F>(&mut self, _f: F)
    where
        F: FnMut(&P) -> bool,
    {
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Default, Clone)]
pub struct InfiniteQueueConfig {}

impl InfiniteQueueConfig {
    pub fn new() -> Self {
        Self {}
    }
}

impl<P> From<InfiniteQueueConfig> for InfiniteQueue<P> {
    fn from(config: InfiniteQueueConfig) -> Self {
        InfiniteQueue::new(config)
    }
}

#[derive(Debug)]
pub struct InfiniteQueue<P> {
    queue: VecDeque<P>,
}

impl<P> InfiniteQueue<P> {
    pub fn new(_config: InfiniteQueueConfig) -> Self {
        debug!("New InfiniteQueue");
        Self {
            queue: VecDeque::new(),
        }
    }
}

impl<P> Default for InfiniteQueue<P> {
    fn default() -> Self {
        Self::new(InfiniteQueueConfig::default())
    }
}

impl<P> PacketQueue<P> for InfiniteQueue<P>
where
    P: Packet,
{
    type Config = InfiniteQueueConfig;

    fn configure(&mut self, _config: Self::Config) {}

    fn enqueue(&mut self, packet: P) {
        self.queue.push_back(packet);
    }

    fn dequeue(&mut self) -> Option<P> {
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

    fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&P) -> bool,
    {
        self.queue.retain(|packet| f(packet));
    }
}

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

impl<P> From<DropTailQueueConfig> for DropTailQueue<P> {
    fn from(config: DropTailQueueConfig) -> Self {
        DropTailQueue::new(config)
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

impl<P> DropTailQueue<P> {
    pub fn new(config: DropTailQueueConfig) -> Self {
        let packet_limit = config.packet_limit;
        let byte_limit = config.byte_limit;
        debug!(?config, "New DropTailQueue");
        Self {
            queue: VecDeque::new(),
            bw_type: config.bw_type,
            packet_limit,
            byte_limit,
            now_bytes: 0,
        }
    }
}

impl<P> Default for DropTailQueue<P> {
    fn default() -> Self {
        Self::new(DropTailQueueConfig::default())
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
            trace!(
                queue_len = self.queue.len(),
                now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) when enqueuing", packet.l3_length(), self.bw_type.extra_length()
            );
        }
    }

    fn dequeue(&mut self) -> Option<P> {
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

    fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&P) -> bool,
    {
        self.queue.retain(|packet| f(packet));
    }
}

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

impl<P> From<DropHeadQueueConfig> for DropHeadQueue<P> {
    fn from(config: DropHeadQueueConfig) -> Self {
        DropHeadQueue::new(config)
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

impl<P> DropHeadQueue<P> {
    pub fn new(config: DropHeadQueueConfig) -> Self {
        let packet_limit = config.packet_limit;
        let byte_limit = config.byte_limit;
        debug!(?config, "New DropHeadQueue");
        Self {
            queue: VecDeque::new(),
            bw_type: config.bw_type,
            packet_limit,
            byte_limit,
            now_bytes: 0,
        }
    }
}

impl<P> Default for DropHeadQueue<P> {
    fn default() -> Self {
        Self::new(DropHeadQueueConfig::default())
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
        self.bw_type = config.bw_type;
    }

    fn is_zero_buffer(&self) -> bool {
        self.packet_limit.is_some_and(|limit| limit == 0)
            || self.byte_limit.is_some_and(|limit| limit == 0)
    }

    fn enqueue(&mut self, packet: P) {
        self.now_bytes += packet.l3_length() + self.bw_type.extra_length();
        self.queue.push_back(packet);
        while self
            .packet_limit
            .is_some_and(|limit| self.queue.len() > limit)
            || self.byte_limit.is_some_and(|limit| self.now_bytes > limit)
        {
            let _packet = self.dequeue().unwrap();
            #[cfg(test)]
            trace!(
                after_queue_len = self.queue.len(),
                after_now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &_packet.as_slice()[0..std::cmp::min(56, _packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) when enqueuing another packet", _packet.l3_length(), self.bw_type.extra_length()
            );
        }
    }

    fn dequeue(&mut self) -> Option<P> {
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

    fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&P) -> bool,
    {
        self.queue.retain(|packet| f(packet));
    }
}

// CoDel Queue Implementation Reference:
// https://github.com/torvalds/linux/blob/v6.6/include/net/codel.h
// https://github.com/ravinet/mahimahi/blob/0bd12164388bc109bbbd8ffa03a09e94adcbec5a/src/packet/codel_packet_queue.cc

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
        CoDelQueue::new(config)
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

impl<P> Default for CoDelQueue<P>
where
    P: Packet,
{
    fn default() -> Self {
        Self::new(CoDelQueueConfig::default())
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

    fn dequeue(&mut self) -> Option<P> {
        match self.queue.pop_front() {
            Some(mut packet) => {
                self.now_bytes -= packet.l3_length() + self.config.bw_type.extra_length();
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
                        trace!("Exit dropping state since packet should not be dropped");
                    } else {
                        while self.dropping && now >= self.drop_next {
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

    fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&P) -> bool,
    {
        self.queue.retain(|packet| f(packet));
    }
}
