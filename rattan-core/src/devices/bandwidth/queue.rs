use crate::devices::Packet;
use etherparse::Ipv4Ecn;
use rand::{rngs::StdRng, RngCore, SeedableRng};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use siphasher::sip::SipHasher13;
use std::borrow::BorrowMut;
use std::collections::VecDeque;
use std::fmt::Debug;
use tokio::time::{Duration, Instant};
use tracing::{debug, trace};

use super::BwType;

#[cfg(feature = "serde")]
fn serde_default<T: Default + PartialEq>(t: &T) -> bool {
    *t == Default::default()
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

    // If the queue is empty, return `None`
    fn dequeue(&mut self) -> Option<P>;
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

    fn enqueue(&mut self, packet: P) {
        if self
            .packet_limit
            .map_or(true, |limit| self.queue.len() < limit)
            && self.byte_limit.map_or(true, |limit| {
                self.now_bytes + packet.l3_length() + self.bw_type.extra_length() <= limit
            })
        {
            self.now_bytes += packet.l3_length() + self.bw_type.extra_length();
            self.queue.push_back(packet);
        } else {
            trace!(
                queue_len = self.queue.len(),
                now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) when enqueue", packet.l3_length(), self.bw_type.extra_length()
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

    fn enqueue(&mut self, packet: P) {
        self.now_bytes += packet.l3_length() + self.bw_type.extra_length();
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
                "Drop packet(l3_len: {}, extra_len: {}) when enqueue another packet", packet.l3_length(), self.bw_type.extra_length()
            )
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
}

// CoDel Queue Implementation Reference:
// https://github.com/torvalds/linux/blob/v6.6/include/net/codel.h
// https://github.com/ravinet/mahimahi/blob/0bd12164388bc109bbbd8ffa03a09e94adcbec5a/src/packet/codel_packet_queue.cc

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(default))]
#[derive(Debug, Clone)]
pub struct CoDelQueueConfig {
    pub packet_limit: Option<usize>, // the maximum number of packets in the queue, or None for unlimited
    pub byte_limit: Option<usize>, // the maximum number of bytes in the queue, or None for unlimited
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub interval: Duration, // width of moving time window
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub target: Duration, // target queue delay
    pub mtu: u32,                  // device MTU, or minimal queue backlog in bytes
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

    fn enqueue(&mut self, packet: P) {
        if self
            .config
            .packet_limit
            .map_or(true, |limit| self.queue.len() < limit)
            && self.config.byte_limit.map_or(true, |limit| {
                self.now_bytes + packet.l3_length() + self.config.bw_type.extra_length() <= limit
            })
        {
            self.now_bytes += packet.l3_length() + self.config.bw_type.extra_length();
            self.queue.push_back(packet);
        } else {
            trace!(
                queue_len = self.queue.len(),
                now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) when enqueue",
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
}

// FQ-CoDel Queue Implementation Reference: Linux kernel implementation
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(default))]
#[derive(Debug, Clone)]
pub struct FqCoDelQueueConfig {
    pub packet_limit: Option<usize>,
    pub byte_limit: Option<usize>,
    pub mtu: u32,

    pub flows_cnt: u32, // Number of flows (default: 1024)
    pub quantum: i32,   // quantuam used for dequeue (default: mtu size)

    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub interval: Duration, // width of moving time window
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub target: Duration, // target queue delay
    pub use_ecn: bool,
    pub drop_batch_size: u32, // Max number of packets to drop when queue is full

    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "serde_default")
    )]
    pub bw_type: BwType,
}

impl Default for FqCoDelQueueConfig {
    fn default() -> Self {
        Self {
            packet_limit: None,
            byte_limit: None,
            mtu: 1500,
            flows_cnt: 1024,
            quantum: 1500,
            interval: Duration::from_millis(100),
            target: Duration::from_millis(5),
            use_ecn: true,
            drop_batch_size: 64,
            bw_type: BwType::default(),
        }
    }
}

impl FqCoDelQueueConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
        mtu: u32,
        flows_cnt: u32,
        quantum: i32,
        interval: Duration,
        target: Duration,
        use_ecn: bool,
        drop_batch_size: u32,
        bw_type: BwType,
    ) -> Self {
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
            mtu,
            flows_cnt,
            quantum,
            interval,
            target,
            use_ecn,
            drop_batch_size,
            bw_type,
        }
    }
}

struct FqCoDelFlowTuple {
    content: [u8; 13],
}

impl FqCoDelFlowTuple {
    const SOURCE_IP: usize = 0;
    const DESTINATION_IP: usize = 4;
    const SOURCE_PORT: usize = 8;
    const DESTINATION_PORT: usize = 10;
    const PROTOCOL: usize = 12;
    fn new(packet: &impl Packet) -> Self {
        let mut temp = Self { content: [0; 13] };
        let header = match packet.ip_hdr() {
            Some(hdr) => hdr,
            None => return temp,
        };

        let ether_header_length: u8 = match packet.ether_hdr() {
            Some(_) => 14,
            None => 0,
        };

        let ip_hdr_length = header.ihl() * 4; // Length in bytes
        let tcp_start_idx = ether_header_length + ip_hdr_length;
        let raw_packet = packet.as_slice();

        temp.content[Self::SOURCE_IP..Self::SOURCE_IP + 4].copy_from_slice(&header.source);
        temp.content[Self::DESTINATION_IP..Self::DESTINATION_IP + 4]
            .copy_from_slice(&header.destination);

        temp.content[Self::SOURCE_PORT..Self::SOURCE_PORT + 2]
            .copy_from_slice(&raw_packet[tcp_start_idx as usize..tcp_start_idx as usize + 2]);
        temp.content[Self::DESTINATION_PORT..Self::DESTINATION_PORT + 2]
            .copy_from_slice(&raw_packet[tcp_start_idx as usize + 2..tcp_start_idx as usize + 4]);

        temp.content[Self::PROTOCOL] = 4;
        temp
    }

    fn into_bytes(self) -> [u8; 13] {
        self.content
    }
}

#[derive(Clone, Debug)]
struct FqCoDelControlBuffer<P>
where
    P: Packet,
{
    ect: Ipv4Ecn,
    packet: P,
}

impl<P> FqCoDelControlBuffer<P>
where
    P: Packet,
{
    fn new(packet: P) -> Self {
        Self {
            ect: Self::classify(&packet),
            packet,
        }
    }

    fn classify(packet: &P) -> Ipv4Ecn {
        let ip_header = match packet.ip_hdr() {
            Some(header) => header,
            None => return Ipv4Ecn::ZERO,
        };

        ip_header.ecn
    }

    fn set_ce(&mut self) -> bool {
        let mut ip_hdr_idx = 0;
        if self.packet.ether_hdr().is_some() {
            ip_hdr_idx += 14;
        }

        if self.packet.ip_hdr().is_none() {
            return false; // No ip header, cannot change.
        }

        let tos_idx = ip_hdr_idx + 1;
        let check_idx = ip_hdr_idx + 10;

        let packet_buffer = self.packet.as_raw_buffer();
        let tos = &mut packet_buffer[tos_idx];

        let ecn = (*tos + 1) & Ipv4Ecn::TRHEE.value();
        /*
         * After the last operation we have (in binary):
         * INET_ECN_NOT_ECT => 01
         * INET_ECN_ECT_1   => 10
         * INET_ECN_ECT_0   => 11
         * INET_ECN_CE      => 00
         */
        if (ecn & 2) == 0 {
            return ecn == 0;
        }

        /*
         * The following gives us:
         * INET_ECN_ECT_1 => check += htons(0xFFFD)
         * INET_ECN_ECT_0 => check += htons(0xFFFE)
         */

        let tos = &mut packet_buffer[tos_idx];
        *tos |= Ipv4Ecn::TRHEE.value();

        // Now we calculate the new checksum
        let check_add: u16 = 0xFFFB + ecn as u16;

        let check_before =
            u16::from_be_bytes([packet_buffer[check_idx], packet_buffer[check_idx + 1]]);

        let mut check_after = check_before + check_add;
        if check_after < check_add {
            check_after += 1;
        }

        let check_after = check_after.to_be_bytes();
        packet_buffer[check_idx..check_idx + 2].copy_from_slice(&check_after);
        self.ect = Ipv4Ecn::TRHEE;
        true
    }
}

// This is essentially a CoDel queue.
#[derive(Debug)]
struct FqCoDelFlow<P>
where
    P: Packet,
{
    queue: VecDeque<FqCoDelControlBuffer<P>>,
    bytes_in_flow: usize,
    packets_in_flow: usize,
    deficit: i32,
    dropping: bool,
    ldelay: Duration,
    first_above_time: Option<Instant>,
    drop_next: Instant,
    count: u32,
    last_count: u32,
}

impl<P> FqCoDelFlow<P>
where
    P: Packet,
{
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            bytes_in_flow: 0,
            packets_in_flow: 0,
            deficit: 0,
            dropping: false,
            ldelay: Duration::ZERO,
            first_above_time: None,
            drop_next: Instant::now(),
            count: 0,
            last_count: 0,
        }
    }

    fn should_drop(
        &mut self,
        packet: &FqCoDelControlBuffer<P>,
        config: &FqCoDelQueueConfig,
    ) -> bool {
        self.ldelay = Instant::now() - packet.packet.get_timestamp();
        if self.ldelay < config.target || self.bytes_in_flow <= config.mtu as usize {
            self.first_above_time = None;
            return false;
        }

        match self.first_above_time {
            Some(first_above_time) => first_above_time.elapsed() >= Duration::ZERO,
            None => {
                self.first_above_time = Some(Instant::now() + config.interval);
                false
            }
        }
    }

    fn control_law(&self, t: Instant, interval: &Duration) -> Instant {
        t + interval.div_f64(f64::sqrt(self.count as f64))
    }

    // Return dequeued packet and dequeued byte
    fn dequeue_from_head(&mut self, packet_count: usize, bytes_count: usize) -> (usize, usize) {
        let mut count = 0;
        let mut byte = 0;
        while count < packet_count || byte < bytes_count {
            let packet = match self.queue.pop_front() {
                Some(pkt) => pkt,
                None => break,
            };

            byte += packet.packet.l3_length();
            count += 1;
        }

        self.bytes_in_flow -= byte;
        self.packets_in_flow -= count;
        self.count += count as u32;
        (count, byte)
    }

    /// Returns whether this flow should be considered a new flow after enqueue
    /// (whether the queue was empty before this enqueue action)
    fn enqueue(&mut self, packet: FqCoDelControlBuffer<P>) -> bool {
        let is_empty = self.queue.is_empty();
        self.bytes_in_flow += packet.packet.l3_length();
        self.packets_in_flow += 1;
        self.queue.push_back(packet);

        is_empty
    }

    fn dequeue(
        &mut self,
        byte_count: &mut usize,
        packet_count: &mut usize,
        config: &FqCoDelQueueConfig,
    ) -> Option<FqCoDelControlBuffer<P>> {
        let mut packet = match self.queue.pop_front() {
            Some(pkt) => {
                self.bytes_in_flow -= pkt.packet.l3_length();
                self.packets_in_flow -= 1;
                *byte_count -= pkt.packet.l3_length();
                *packet_count -= 1;
                pkt
            }
            None => {
                self.dropping = false;
                return None;
            }
        };

        let now = Instant::now();
        let drop = self.should_drop(&packet, config);

        if self.dropping {
            if drop {
                /* sojourn time below target - leave dropping state */
                self.dropping = false;
            } else {
                while self.dropping && now >= self.drop_next {
                    self.count += 1;
                    if config.use_ecn && packet.ect != Ipv4Ecn::ZERO {
                        self.drop_next = self.control_law(self.drop_next, &config.interval);
                        packet.set_ce();
                        break;
                    }

                    packet = match self.queue.pop_front() {
                        Some(pkt) => {
                            self.bytes_in_flow -= pkt.packet.l3_length();
                            self.packets_in_flow -= 1;
                            *byte_count -= pkt.packet.l3_length();
                            *packet_count -= 1;
                            pkt
                        }
                        None => {
                            self.dropping = false;
                            return None;
                        }
                    };

                    if self.should_drop(&packet, config) {
                        self.drop_next = self.control_law(self.drop_next, &config.interval);
                    } else {
                        self.dropping = false;
                    }
                }
            }
        } else if drop {
            if config.use_ecn && packet.ect != Ipv4Ecn::ZERO {
                packet.set_ce();
            } else {
                packet = match self.queue.pop_front() {
                    Some(pkt) => {
                        self.bytes_in_flow -= pkt.packet.l3_length();
                        self.packets_in_flow -= 1;
                        *byte_count -= pkt.packet.l3_length();
                        *packet_count -= 1;
                        pkt
                    }
                    None => {
                        self.dropping = false;
                        return None;
                    }
                };

                // This is for status update
                let _should_drop = self.should_drop(&packet, config);
            }

            self.dropping = true;
            let delta = self.count - self.last_count;

            if delta > 1 && now - self.drop_next < 16 * config.interval {
                self.count = delta;
            } else {
                self.count = 1;
            }

            self.last_count = self.count;
            self.drop_next = self.control_law(now, &config.interval);
        }

        Some(packet)
    }
}
#[derive(Debug)]
pub struct FqCoDelQueue<P>
where
    P: Packet,
{
    config: FqCoDelQueueConfig,
    flows: Vec<FqCoDelFlow<P>>,
    new_flows: VecDeque<u32>, // The index of new flows
    old_flows: VecDeque<u32>, // The index of old flows
    bytes_in_queue: usize,
    packets_in_queue: usize,

    hasher: SipHasher13,
}

impl<P> FqCoDelQueue<P>
where
    P: Packet,
{
    pub fn new(config: FqCoDelQueueConfig) -> Self {
        let mut rng = StdRng::seed_from_u64(42);
        let flows: Vec<FqCoDelFlow<_>> = (0..config.flows_cnt)
            .map(|_| FqCoDelFlow::<P>::new())
            .collect();

        Self {
            config,
            flows,
            new_flows: VecDeque::new(),
            old_flows: VecDeque::new(),
            bytes_in_queue: 0,
            packets_in_queue: 0,

            hasher: SipHasher13::new_with_key(
                &(((rng.next_u64() as u128) << 64) + rng.next_u64() as u128).to_le_bytes(),
            ),
        }
    }

    fn fq_codel_drop(&mut self) {
        let mut max_backlog = 0;
        let mut idx = 0;
        for i in 0..self.config.flows_cnt as usize {
            if self.flows[i].bytes_in_flow > max_backlog {
                max_backlog = self.flows[i].bytes_in_flow;
                idx = i;
            }
        }

        /* Our goal is to drop half of this fat flow backlog */
        let threshold = max_backlog >> 1;

        // let mut flow = self.flows[idx].blocking_lock();
        let (dequeued_count, dequeued_byte) =
            self.flows[idx].dequeue_from_head(self.config.drop_batch_size as usize, threshold);

        self.bytes_in_queue -= dequeued_byte;
        self.packets_in_queue -= dequeued_count;
    }
}

impl<P> PacketQueue<P> for FqCoDelQueue<P>
where
    P: Packet,
{
    type Config = FqCoDelQueueConfig;

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn enqueue(&mut self, packet: P) {
        if self
            .config
            .packet_limit
            .map_or(false, |limit| self.packets_in_queue >= limit)
            || self.config.byte_limit.map_or(false, |limit| {
                self.bytes_in_queue + packet.l3_length() + self.config.bw_type.extra_length()
                    > limit
            })
        {
            // Over limit
            self.fq_codel_drop();
            return;
        }

        let flow_tuple = FqCoDelFlowTuple::new(&packet);
        let tuple_bytes = flow_tuple.into_bytes();
        let hash = self.hasher.hash(&tuple_bytes) as u32;
        let idx = ((hash as u64 * self.config.flows_cnt as u64) >> 32) as u32;

        let mut control_buff = FqCoDelControlBuffer::new(packet);
        control_buff.packet.set_timestamp(Instant::now());

        self.bytes_in_queue += control_buff.packet.l3_length() + self.config.bw_type.extra_length();
        self.packets_in_queue += 1;
        let is_new = self.flows[idx as usize].enqueue(control_buff);
        if is_new {
            self.flows[idx as usize].deficit = self.config.quantum;
            self.new_flows.push_back(idx);
        }
    }

    fn dequeue(&mut self) -> Option<P> {
        loop {
            let (flow_idx, from_new) = match self.new_flows.pop_front() {
                Some(idx) => (idx, true),
                None => match self.old_flows.pop_front() {
                    Some(idx) => (idx, false),
                    None => return None,
                },
            };

            let flow = self.flows[flow_idx as usize].borrow_mut();
            if flow.deficit <= 0 {
                flow.deficit += self.config.quantum;
                self.old_flows.push_back(flow_idx);
                continue;
            }

            let dequeued_packet = match flow.dequeue(
                &mut self.bytes_in_queue,
                &mut self.packets_in_queue,
                &self.config,
            ) {
                Some(pkt) => {
                    flow.deficit -= pkt.packet.l3_length() as i32;
                    pkt
                }
                None => {
                    if from_new && !self.old_flows.is_empty() {
                        // Prevent the starvation of old queues
                        self.old_flows.push_back(flow_idx);
                    }
                    // Is there is also no flow in old flows, simply drop this flow
                    continue;
                }
            };
            /* The c code is actually peeking the old/new flow queue. If we got
             * here, we have to put it back to the front of what it belongs
             */
            if from_new {
                self.new_flows.push_front(flow_idx);
            } else {
                self.old_flows.push_front(flow_idx);
            }
            return Some(dequeued_packet.packet);
        }
    }
}

// DualPI2 Queue Implementation Reference:
// https://github.com/L4STeam/linux/blob/testing/net/sched/sch_dualpi2.c

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone)]
pub enum ECNThreshHold {
    Packets(u32),
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    Delay(Duration),
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone)]
/// Mask used to determine whether a packet with specific ECN mark should go into L4S queue or
/// classic queue, or whether a packet with specific ECN mark should be marked CE with
/// probability p (treated as scalable congestion control) or p^2 (treated as classic
/// congestion control).
///
/// `NoECN` means all ECN codepoints are treated as if they are classic packets.
///
/// `L4SECT` means only ECT(1) and CE are treated with L4S specifications.
///
/// `AnyECT` means ECT(0), ECT(1) and CE are all treated with L4S specifications, for compatibility with DCTCP.
pub enum EcnCodePointMask {
    NoECN,  // No ECN, 0 (00)
    L4SECT, // Only ECT(1) and CE, 1 (01)
    AnyECT, // ECT(0), ECT(1) and CE, for compatibility with DCTCP, 3 (11)
}

impl EcnCodePointMask {
    fn value(&self) -> u8 {
        match self {
            Self::NoECN => 0,
            Self::L4SECT => 1,
            Self::AnyECT => 3,
        }
    }
}

/// Describs which queue to dequeue from
type QueueCredit = i32;

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(default))]
#[derive(Debug, Clone)]
pub struct DualPI2QueueConfig {
    pub packet_limit: Option<usize>, // number of packets can be enqueued (L4S + Classic), None for unlimited
    pub byte_limit: Option<usize>,   // number of bytes can be enqueued, None for unlimited

    pub mtu: u32,

    /* PI2 parameters */
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub target: Duration, // User specified target delay
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub tupdate: Duration, // timer frequency
    pub alpha: u32, // Gain factor for integral rate response
    pub beta: u32,  // Gain factor for proportional response

    /* Step AQM (L4S queue) parameters */
    pub ecn_thresh: ECNThreshHold, // threshold sojourn time / queue size to mark L4S queue packets

    /* Classic queue starvation protection */
    pub c_protection_wc: u8,

    /* General DualQ parameters */
    pub coupling_factor: u8, // coupling rate between Classic and L4S
    pub ecn_mask: EcnCodePointMask, /* Mask on ecn bits to determine if packet goes in l-queue
                             0 (00) - NoECN: single queue
                             1 (01) - L4SECT: dual queue for ECT(1) and CE
                             3 (11) - AnyECT: dual queue for ECT(0) , ECT(1) and CE (this is for DCTCP compatibility)
                             */
    pub drop_early: bool,
    pub drop_overload: bool, // Drop on overload (true) or overflow (false)
}

impl Default for DualPI2QueueConfig {
    fn default() -> Self {
        Self {
            packet_limit: None,
            byte_limit: None,
            mtu: 1500,
            target: Duration::from_millis(15),
            tupdate: Duration::from_millis(16),
            alpha: Self::scale_alpha_beta(41), // ~0.16Hz * 256
            beta: Self::scale_alpha_beta(819), // ~3.20Hz * 256
            ecn_thresh: ECNThreshHold::Delay(Duration::from_millis(1)),
            c_protection_wc: 10,
            coupling_factor: 2,
            ecn_mask: EcnCodePointMask::L4SECT,
            drop_early: false,
            drop_overload: true,
        }
    }
}

impl DualPI2QueueConfig {
    /* The max_prob must match the rng */
    const MAX_PROB: u32 = 0xffffffff;

    /* alpha/beta values exchanged are in units of 256ns */
    const ALPHA_BETA_SHIFT: u32 = 8;
    /* Scaled values of alpha/beta must fit in 32b to avoid overflow in later
    computations. Consequently (see and dualpi2_scale_alpha_beta()), their
    netlink-provided values can use at most 31b, i.e. be at most most (2^23)-1
    (~4MHz) as those are given in 1/256th. This enable to tune alpha/beta to
    control flows whose maximal RTTs can be in usec up to few secs.
    */
    /* Internal alpha/beta are in units of 64ns.
    This enables to use all alpha/beta values in the allowed range without loss
    of precision due to rounding when scaling them internally, e.g.,
    scale_alpha_beta(1) will not round down to 0.
    */
    const ALPHA_BETA_GRANULARITY: u32 = 6;
    const ALPHA_BETA_SCALING: u32 = Self::ALPHA_BETA_SHIFT - Self::ALPHA_BETA_GRANULARITY;
    /* We express the weights (wc, wl) in %, i.e., wc + wl = 100 */
    const MAX_WC: u8 = 100;

    const MAX_ECN_CLASSIFY: u8 = 3;

    // The `new` method requires this many arguments and their meanings are quite
    // straight forward
    #[allow(clippy::too_many_arguments)]
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
        mtu: u32,
        target: Duration,
        tupdate: Duration,
        alpha: u32,
        beta: u32,
        ecn_thresh: ECNThreshHold,
        c_protection_wc: u8,
        coupling_factor: u8,
        ecn_mask: EcnCodePointMask,
        drop_early: bool,
        drop_overload: bool,
    ) -> Self {
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
            mtu,
            target,
            tupdate,
            alpha: Self::scale_alpha_beta(alpha),
            beta: Self::scale_alpha_beta(beta),
            ecn_thresh,
            c_protection_wc,
            coupling_factor,
            ecn_mask,
            drop_early,
            drop_overload,
        }
    }

    fn scale_alpha_beta(param: u32) -> u32 {
        // The 1e9 is NSEC_IN_SEC
        ((param as u64 * (Self::MAX_PROB >> Self::ALPHA_BETA_SCALING) as u64) / 1000000000) as u32
    }
}

#[derive(Debug, Clone, PartialEq)]
enum DualPI2ClassificationResult {
    Classic, // C queue
    L4S,     // L queue (scalable marking/classic drops)
    LNoDrop, // L queue (no drops/marks)
}

impl From<u8> for DualPI2ClassificationResult {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Classic,
            1 => Self::L4S,
            2 => Self::LNoDrop,
            _ => Self::Classic,
        }
    }
}

#[derive(Debug, Clone)]
struct DualPI2PacketControlBuffer<P> {
    apply_step: bool,
    ect: Ipv4Ecn,
    classified_result: DualPI2ClassificationResult,
    packet: P, // The actual packet
}

impl<P> DualPI2PacketControlBuffer<P>
where
    P: Packet,
{
    fn new(packet: P) -> Self {
        Self {
            apply_step: false,
            ect: Ipv4Ecn::ZERO,
            classified_result: DualPI2ClassificationResult::Classic,
            packet,
        }
    }

    fn classify(&mut self, mask: &EcnCodePointMask) {
        let ip_header = self.packet.ip_hdr();
        let ip_header = match ip_header {
            Some(header) => header,
            None => {
                self.classified_result = DualPI2ClassificationResult::Classic;
                return;
            }
        };
        self.ect = ip_header.ecn;

        if self.ect.value() & mask.value() != 0 {
            self.classified_result = DualPI2ClassificationResult::L4S;
            return;
        }

        if self.ect.value() < DualPI2QueueConfig::MAX_ECN_CLASSIFY {
            self.classified_result = DualPI2ClassificationResult::from(self.ect.value());
            return;
        }

        self.classified_result = DualPI2ClassificationResult::Classic;
    }

    fn in_l_queue(&self) -> bool {
        self.classified_result != DualPI2ClassificationResult::Classic
    }

    fn is_l4s(&self) -> bool {
        self.classified_result == DualPI2ClassificationResult::L4S
    }

    fn set_ce(&mut self) -> bool {
        let mut ip_hdr_idx = 0;
        if self.packet.ether_hdr().is_some() {
            ip_hdr_idx += 14;
        }

        if self.packet.ip_hdr().is_none() {
            return false; // No ip header, cannot change.
        }

        let tos_idx = ip_hdr_idx + 1;
        let check_idx = ip_hdr_idx + 10;

        let packet_buffer = self.packet.as_raw_buffer();
        let tos = &mut packet_buffer[tos_idx];

        let ecn = (*tos + 1) & Ipv4Ecn::TRHEE.value();
        /*
         * After the last operation we have (in binary):
         * INET_ECN_NOT_ECT => 01
         * INET_ECN_ECT_1   => 10
         * INET_ECN_ECT_0   => 11
         * INET_ECN_CE      => 00
         */
        if (ecn & 2) == 0 {
            return ecn == 0;
        }

        /*
         * The following gives us:
         * INET_ECN_ECT_1 => check += htons(0xFFFD)
         * INET_ECN_ECT_0 => check += htons(0xFFFE)
         */

        let tos = &mut packet_buffer[tos_idx];
        *tos |= Ipv4Ecn::TRHEE.value();

        // Now we calculate the new checksum
        let check_add: u16 = 0xFFFB + ecn as u16;

        let check_before =
            u16::from_be_bytes([packet_buffer[check_idx], packet_buffer[check_idx + 1]]);

        let mut check_after = check_before + check_add;
        if check_after < check_add {
            check_after += 1;
        }

        let check_after = check_after.to_be_bytes();
        packet_buffer[check_idx] = check_after[0];
        packet_buffer[check_idx + 1] = check_after[1];
        true
    }
}

#[derive(Debug)]
pub struct DualPI2Queue<P>
where
    P: Packet,
{
    c_queue: VecDeque<DualPI2PacketControlBuffer<P>>, // Classic queue
    l_queue: VecDeque<DualPI2PacketControlBuffer<P>>, // L4S queue

    config: DualPI2QueueConfig,

    /* PI2 parameters */
    pi2_prob: u32,             // Base PI probability
    last_prob_update: Instant, // PI2 prob update time

    /* Classic queue starvation protection */
    c_protection_credit: QueueCredit,
    c_protection_init: QueueCredit,
    // wc is in config, and wl can be always be calculated through WC_MAX - wc
    c_protection_wl: u8,

    /* Statistics */
    c_head_timestamp: Option<Instant>, // Enqueue timestamp of the classic queue's head, None when queue is empty
    l_head_timestamp: Option<Instant>, // Enqueue timestamp of the L4S queue's head, None when queue is empty
    last_qdelay: Duration,             // Queue delay at the last probability update
    packets_in_c: usize,
    bytes_in_c: usize,
    packets_in_l: usize,
    bytes_in_l: usize,
    ecn_mark: u32,   // packets marked with ECN
    step_marks: u32, // ECN marks due to the step AQM

    /* Defferred drop statistics */
    defferred_dropped_cnt: u32, // Packets dropped
    defferred_dropped_len: u32, // Bytes dropped

    rng: StdRng,
}

impl<P> From<DualPI2QueueConfig> for DualPI2Queue<P>
where
    P: Packet,
{
    fn from(config: DualPI2QueueConfig) -> Self {
        DualPI2Queue::new(config)
    }
}

impl<P> PacketQueue<P> for DualPI2Queue<P>
where
    P: Packet,
{
    type Config = DualPI2QueueConfig;
    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn enqueue(&mut self, packet: P) {
        // We have no system timer, thus we all update_prob on every enqueue and
        // dequeue to ensure the update of the prob
        self.update_prob();

        let mut control_buffer = DualPI2PacketControlBuffer::new(packet);

        control_buffer.classify(&self.config.ecn_mask);

        // If the queue has reached limit, drop the packet
        if let Some(limit) = self.config.packet_limit {
            if self.packets_in_c + self.packets_in_l >= limit {
                // Drop the packet
                return;
            }
        }

        if let Some(limit) = self.config.byte_limit {
            if self.bytes_in_c + self.bytes_in_l >= limit {
                // Drop the packet
                return;
            }
        }

        if self.config.drop_early && self.must_drop(&mut control_buffer) {
            // Drop the packet
            return;
        }

        control_buffer.packet.set_timestamp(Instant::now());
        if control_buffer.in_l_queue() {
            // Only apply the step if a queue is building up
            control_buffer.apply_step = control_buffer.is_l4s() && self.l_queue.len() > 1;
            self.packets_in_l += 1;
            self.bytes_in_l += control_buffer.packet.l3_length();
            if self.l_head_timestamp.is_none() {
                self.l_head_timestamp = Some(control_buffer.packet.get_timestamp());
            }
            self.l_queue.push_back(control_buffer);
            return;
        }

        self.packets_in_c += 1;
        self.bytes_in_c += control_buffer.packet.l3_length();
        if self.c_head_timestamp.is_none() {
            self.c_head_timestamp = Some(control_buffer.packet.get_timestamp());
        }
        self.c_queue.push_back(control_buffer);
    }

    fn dequeue(&mut self) -> Option<P> {
        // We have no system timer, thus we all update_prob on every enqueue and
        // dequeue to ensure the update of the prob
        self.update_prob();
        let now = Instant::now();

        loop {
            let (packet_cb, credit_change) = self.dequeue_packet();
            self.c_protection_credit += credit_change;
            match packet_cb {
                Some(mut packet_cb) => {
                    if (!self.config.drop_early && self.must_drop(&mut packet_cb))
                        || (packet_cb.in_l_queue() && self.do_step_aqm(&mut packet_cb, now))
                    {
                        self.defferred_dropped_cnt += 1;
                        self.defferred_dropped_len += packet_cb.packet.l3_length() as u32;

                        // Drop the packet
                        continue;
                    }
                    return Some(packet_cb.packet);
                }
                None => return None,
            }
        }
    }
}

impl<P> DualPI2Queue<P>
where
    P: Packet,
{
    pub fn new(config: DualPI2QueueConfig) -> Self {
        debug!(?config, "New DualPI2Queue");
        let mut temp = Self {
            c_queue: VecDeque::new(),
            l_queue: VecDeque::new(),

            config,

            pi2_prob: 0,
            last_prob_update: Instant::now(),

            c_protection_credit: 0,
            c_protection_init: 0,
            c_protection_wl: 0,

            c_head_timestamp: None,
            l_head_timestamp: None,
            last_qdelay: Duration::ZERO,
            packets_in_c: 0,
            bytes_in_c: 0,
            packets_in_l: 0,
            bytes_in_l: 0,
            ecn_mark: 0,
            step_marks: 0,

            defferred_dropped_cnt: 0,
            defferred_dropped_len: 0,

            rng: StdRng::seed_from_u64(42),
        };
        temp.calculate_c_protection();
        debug!("Initialized DualPI2: {:?}", temp);
        temp
    }

    fn reset_c_protection(&mut self) {
        self.c_protection_credit = self.c_protection_init;
    }

    fn calculate_c_protection(&mut self) {
        self.c_protection_wl = DualPI2QueueConfig::MAX_WC - self.config.c_protection_wc;
        self.c_protection_init = self.config.mtu as i32
            * (self.config.c_protection_wc as i32 - self.c_protection_wl as i32);
        self.reset_c_protection();
    }

    fn must_drop(&mut self, packet_control_buffer: &mut DualPI2PacketControlBuffer<P>) -> bool {
        /* Never drop packets  */
        if self.bytes_in_c + self.bytes_in_l < 2 * self.config.mtu as usize {
            return false;
        }

        trace!("DaulPI2 prob: {}", self.pi2_prob);

        let prob = self.pi2_prob;
        let local_l_prob = prob as u64 * self.config.coupling_factor as u64;
        let overload = local_l_prob > DualPI2QueueConfig::MAX_PROB as u64;

        match packet_control_buffer.classified_result {
            DualPI2ClassificationResult::Classic => {
                if self.roll(prob) && self.roll(prob) {
                    if overload || packet_control_buffer.ect == Ipv4Ecn::ZERO {
                        return true;
                    }
                    let _ = self.mark(packet_control_buffer);
                }
                false
            }
            DualPI2ClassificationResult::L4S => {
                if overload {
                    // Apply classic drop
                    // Logic for better understanding:
                    // !self.config.drop_overload || !(self.roll(prob) && self.roll(prob))
                    if !(self.config.drop_overload && self.roll(prob) && self.roll(prob)) {
                        let _ = self.mark(packet_control_buffer);
                        return false;
                    }
                    return true;
                }

                // We can safely cut the upper 32b as overload == false
                if self.roll(local_l_prob as u32) {
                    if packet_control_buffer.ect == Ipv4Ecn::ZERO {
                        return true;
                    }
                    let _ = self.mark(packet_control_buffer);
                }
                false
            }
            DualPI2ClassificationResult::LNoDrop => false,
        }
    }

    fn roll(&mut self, prob: u32) -> bool {
        self.rng.next_u32() <= prob
    }

    fn mark(
        &mut self,
        packet_control_buffer: &mut DualPI2PacketControlBuffer<P>,
    ) -> Result<(), ()> {
        if packet_control_buffer.set_ce() {
            self.ecn_mark += 1;
            Ok(())
        } else {
            Err(())
        }
    }

    fn dequeue_packet(&mut self) -> (Option<DualPI2PacketControlBuffer<P>>, i32) {
        /* Prioritize dequeing from the L queue. Only protection credit is > 0
        we try to give out c queue packet. If C queue has no packet when it is
        given the chance the dequeue, we still try to dequeue from L; similarly,
        if there is no packet to dequeue in L queue when it is given the chance
        we will try to dequeue from C queue */
        let mut credit_change = 0;
        trace!(
            "Dequeue: l length {}, c length {}, credit {}",
            self.l_queue.len(),
            self.c_queue.len(),
            self.c_protection_credit
        );
        let packet = if (self.c_queue.is_empty() || self.c_protection_credit <= 0)
            && !self.l_queue.is_empty()
        {
            let dequeued_packet = self
                .l_queue
                .pop_front()
                .expect("If branch already checked none empty");

            self.l_head_timestamp = self
                .l_queue
                .front()
                .map(|front_pkt| front_pkt.packet.get_timestamp());

            if !self.c_queue.is_empty() {
                credit_change = self.config.c_protection_wc as i32;
            }

            self.packets_in_l -= 1;
            self.bytes_in_l -= dequeued_packet.packet.l3_length();
            dequeued_packet
        } else if !self.c_queue.is_empty() {
            let dequeued_packet = self
                .c_queue
                .pop_front()
                .expect("If branch already checked none empty");

            self.c_head_timestamp = self
                .c_queue
                .front()
                .map(|front_pkt| front_pkt.packet.get_timestamp());

            if !self.l_queue.is_empty() {
                credit_change = -(self.c_protection_wl as i32);
            }

            self.packets_in_c -= 1;
            self.bytes_in_c -= dequeued_packet.packet.l3_length();
            dequeued_packet
        } else {
            self.reset_c_protection();
            return (None, 0);
        };
        // This is not safe only in theory. l3_length gives usize, which may
        // overflow the i32. However, I fail to come up with a scenario where the
        // packet size will be larger then 2^31 - 1, thus this in practical should
        // never cause any problem.
        credit_change *= packet.packet.l3_length() as i32;
        (Some(packet), credit_change)
    }

    fn do_step_aqm(&mut self, packet: &mut DualPI2PacketControlBuffer<P>, now: Instant) -> bool {
        let over_thresh = match self.config.ecn_thresh {
            ECNThreshHold::Packets(thresh) => self.packets_in_l > thresh as usize,
            ECNThreshHold::Delay(thresh) => {
                let sojourn_time = now - packet.packet.get_timestamp();
                sojourn_time > thresh
            }
        };

        if packet.apply_step && over_thresh {
            if packet.ect == Ipv4Ecn::ZERO {
                // Drop this non - ECT packet
                return true;
            }
            if self.mark(packet).is_ok() {
                self.step_marks += 1;
            }
        }
        false
    }

    fn update_prob(&mut self) {
        while self.last_prob_update.elapsed() >= self.config.tupdate {
            self.last_prob_update += self.config.tupdate;
            self.pi2_prob = self.calculate_prob(self.last_prob_update);
        }
    }

    fn calculate_prob(&mut self, time_ref: Instant) -> u32 {
        let qdelay_c = if let Some(timestamp) = self.c_head_timestamp {
            time_ref - timestamp
        } else {
            Duration::ZERO
        };

        let qdelay_l = if let Some(timestamp) = self.l_head_timestamp {
            time_ref - timestamp
        } else {
            Duration::ZERO
        };

        let qdelay = if qdelay_c > qdelay_l {
            qdelay_c
        } else {
            qdelay_l
        };

        /* Alpha and beta take at most 32b, i.e, the delay difference would
        overflow for queuing delay differences > ~4.2sec.
        */
        let mut delta = (qdelay.as_nanos() as i64 - self.config.target.as_nanos() as i64)
            * self.config.alpha as i64;
        delta += (qdelay.as_nanos() as i64 - self.last_qdelay.as_nanos() as i64)
            * self.config.beta as i64;
        let new_prob = self.pi2_prob as i64 + Self::scale_delta(delta);
        let new_prob: u32 = match new_prob {
            p if p > u32::MAX as i64 => u32::MAX,
            p if p < 0 => 0,
            _ => new_prob as u32,
        };
        self.last_qdelay = qdelay;
        /* If we do not drop on overload, ensure we cap the L4S probability to
         * 100% to keep window fairness when overflowing.
         */
        if !self.config.drop_overload {
            if new_prob > DualPI2QueueConfig::MAX_PROB / self.config.coupling_factor as u32 {
                return DualPI2QueueConfig::MAX_PROB / self.config.coupling_factor as u32;
            } else {
                return new_prob;
            }
        }
        new_prob
    }

    fn scale_delta(delta: i64) -> i64 {
        delta / (1 << DualPI2QueueConfig::ALPHA_BETA_GRANULARITY)
    }
}

/// PIE AQM implementation. Ref: linux kernel implementation
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(default))]
#[derive(Debug, Clone)]
pub struct PIEQueueConfig {
    pub packet_limit: Option<usize>,
    pub byte_limit: Option<usize>,
    pub mtu: u32,

    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub target: Duration, // AQM Latency Target (default: 15 milliseconds)
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub t_update: Duration, // A period to calculate drop prob (default: 15 milliseconds)

    pub alpha: u64, // For prob calculation. Use a value of 0-32 and it will be scaled down to value between 0-2 (default: 2, scaled to 1/8=0.125). Internally stores the value after scaling.
    pub beta: u64, // For prob calculation. Use a value of 0-32 and it will be scaled down to value between 0-2 (default: 20, scaled to 1 + 1/4). Internally stores the value after scaling

    pub use_ecn: bool,           // is ECN marking of packets enabled
    pub bytemode: bool,          // is drop probability scaled based on pkt size
    pub dq_rate_estimator: bool, // is Little's law used for qdelay calculation, if not, use jojourn time as qdelay

    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "serde_default")
    )]
    pub bw_type: BwType,
}

impl Default for PIEQueueConfig {
    fn default() -> Self {
        Self {
            packet_limit: None,
            byte_limit: None,
            mtu: 1500,
            target: Duration::from_millis(15),
            t_update: Duration::from_millis(15),
            alpha: Self::scale_alpha_beta(8),
            beta: Self::scale_alpha_beta(20),
            use_ecn: false,
            bytemode: false,
            dq_rate_estimator: false,
            bw_type: BwType::default(),
        }
    }
}

impl PIEQueueConfig {
    const MAX_PROB: u64 = u64::MAX >> 8;
    const QUEUE_THRESHOLD: u32 = 16384;
    const PIE_SCALE: u8 = 8;

    #[allow(clippy::too_many_arguments)]
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
        mtu: u32,
        target: Duration,
        t_update: Duration,
        alpha: u8,
        beta: u8,
        use_ecn: bool,
        bytemode: bool,
        dq_rate_estimator: bool,
        bw_type: BwType,
    ) -> Self {
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
            mtu,
            target,
            t_update,
            alpha: Self::scale_alpha_beta(alpha),
            beta: Self::scale_alpha_beta(beta),
            use_ecn,
            bytemode,
            dq_rate_estimator,
            bw_type,
        }
    }

    fn scale_alpha_beta(coeff: u8) -> u64 {
        (coeff as u64 * (PIEQueueConfig::MAX_PROB / (1000000000 >> 6))) >> 4
    }
}

impl<P> From<PIEQueueConfig> for PIEQueue<P>
where
    P: Packet,
{
    fn from(config: PIEQueueConfig) -> Self {
        PIEQueue::new(config)
    }
}

#[derive(Debug, Clone)]
struct PIEPacketControlBuffer<P> {
    ect: Ipv4Ecn,
    packet: P,
}

impl<P> PIEPacketControlBuffer<P>
where
    P: Packet,
{
    fn new(packet: P) -> Self {
        Self {
            ect: Self::classify(&packet),
            packet,
        }
    }

    fn classify(packet: &P) -> Ipv4Ecn {
        let ip_header = match packet.ip_hdr() {
            Some(header) => header,
            None => return Ipv4Ecn::ZERO,
        };

        ip_header.ecn
    }

    fn set_ce(&mut self) -> bool {
        let mut ip_hdr_idx = 0;
        if self.packet.ether_hdr().is_some() {
            ip_hdr_idx += 14;
        }

        if self.packet.ip_hdr().is_none() {
            return false; // No ip header, cannot change.
        }

        let tos_idx = ip_hdr_idx + 1;
        let check_idx = ip_hdr_idx + 10;

        let packet_buffer = self.packet.as_raw_buffer();
        let tos = &mut packet_buffer[tos_idx];

        let ecn = (*tos + 1) & Ipv4Ecn::TRHEE.value();
        /*
         * After the last operation we have (in binary):
         * INET_ECN_NOT_ECT => 01
         * INET_ECN_ECT_1   => 10
         * INET_ECN_ECT_0   => 11
         * INET_ECN_CE      => 00
         */
        if (ecn & 2) == 0 {
            return ecn == 0;
        }

        /*
         * The following gives us:
         * INET_ECN_ECT_1 => check += htons(0xFFFD)
         * INET_ECN_ECT_0 => check += htons(0xFFFE)
         */

        let tos = &mut packet_buffer[tos_idx];
        *tos |= Ipv4Ecn::TRHEE.value();

        // Now we calculate the new checksum
        let check_add: u16 = 0xFFFB + ecn as u16;

        let check_before =
            u16::from_be_bytes([packet_buffer[check_idx], packet_buffer[check_idx + 1]]);

        let mut check_after = check_before + check_add;
        if check_after < check_add {
            check_after += 1;
        }

        let check_after = check_after.to_be_bytes();
        packet_buffer[check_idx..check_idx + 2].copy_from_slice(&check_after);
        // packet_buffer[check_idx] = check_after[0];
        // packet_buffer[check_idx + 1] = check_after[1];
        self.ect = Ipv4Ecn::TRHEE;
        true
    }
}

#[derive(Debug)]
pub struct PIEQueue<P>
where
    P: Packet,
{
    queue: VecDeque<PIEPacketControlBuffer<P>>,
    config: PIEQueueConfig,

    qdelay: Duration,
    qdelay_old: Duration,
    burst_time: Duration,
    dq_timestamp: Instant, // timestamp at which dq rate was last calculated
    prob: u64,
    accu_prob: u64,
    dq_count: Option<u64>,
    avg_dq_rate: u32,
    backlog_old: usize,

    packets_in_q: u32,
    bytes_in_q: usize,

    last_update_time: Instant,

    rng: StdRng,
}

impl<P> Default for PIEQueue<P>
where
    P: Packet,
{
    fn default() -> Self {
        Self::new(PIEQueueConfig::default())
    }
}

impl<P> PIEQueue<P>
where
    P: Packet,
{
    pub fn new(config: PIEQueueConfig) -> Self {
        debug!(?config, "New PIEQueue");
        Self {
            queue: VecDeque::new(),
            config,

            qdelay: Duration::ZERO,
            qdelay_old: Duration::ZERO,
            burst_time: Duration::from_millis(150),
            dq_timestamp: Instant::now(),
            prob: 0,
            accu_prob: 0,
            dq_count: None,
            avg_dq_rate: 0,
            backlog_old: 0,

            packets_in_q: 0,
            bytes_in_q: 0,

            last_update_time: Instant::now(),

            rng: StdRng::seed_from_u64(42),
        }
    }

    fn drop_early(&mut self, packet_size: u64) -> bool {
        /* If there is still burst allowance left skip random early drop */
        if self.burst_time > Duration::ZERO {
            return false;
        }

        /* If currentdelay is less than half of target, and if drop prob is low already,
         * disable early_drop
         */
        if (self.qdelay < self.config.target / 2) && self.prob < PIEQueueConfig::MAX_PROB / 5 {
            return false;
        }

        /* If we have fewer than 2 mtu-sized packets, disable pie_drop_early,
         * similar to min_th in RED
         */
        if self.bytes_in_q < 2 * self.config.mtu as usize {
            return false;
        }

        /* If bytemode is turned on, use packet size to compute new
         * probablity. Smaller packets will have lower drop prob in this case
         */
        let local_prob = if self.config.bytemode && packet_size < self.config.mtu as u64 {
            packet_size * (self.prob / self.config.mtu as u64)
        } else {
            self.prob
        };

        if local_prob == 0 {
            self.accu_prob = 0;
        } else {
            self.accu_prob += local_prob;
        }

        if self.accu_prob < (PIEQueueConfig::MAX_PROB / 100) * 85 {
            return false;
        }

        if self.accu_prob >= (PIEQueueConfig::MAX_PROB / 2) * 17 {
            return true;
        }

        let rand = self.rng.next_u64();
        if (rand >> 8) < local_prob {
            self.accu_prob = 0;
            return true;
        }

        false
    }

    fn update_prob(&mut self) {
        if self.last_update_time.elapsed() >= self.config.t_update {
            self.prob = self.calculate_prob();
        }
        while self.last_update_time.elapsed() >= self.config.t_update {
            self.last_update_time += self.config.t_update;
        }
    }

    fn calculate_prob(&mut self) -> u64 {
        // I simply copied the logic from the original code...
        let mut update_prob = true;
        let qdelay: Duration;
        let qdelay_old: Duration;

        if self.config.dq_rate_estimator {
            qdelay_old = self.qdelay;
            self.qdelay_old = self.qdelay;

            if self.avg_dq_rate > 0 {
                qdelay = Duration::from_nanos(
                    (((self.bytes_in_q << PIEQueueConfig::PIE_SCALE) / self.avg_dq_rate as usize)
                        << 6) as u64,
                );
            } else {
                qdelay = Duration::ZERO;
            }
        } else {
            qdelay = self.qdelay;
            qdelay_old = self.qdelay_old;
        }

        /* If qdelay is zero and backlog is not, it means backlog is very small,
         * so we do not update probability in this round.
         */
        if qdelay.is_zero() && self.bytes_in_q != 0 {
            update_prob = false;
        }

        /* In the algorithm, alpha and beta are between 0 and 2 with typical
         * value for alpha as 0.125. In this implementation, we use values 0-32
         * passed from user space to represent this. Also, alpha and beta have
         * unit of HZ and need to be scaled before they can used to update
         * probability. alpha/beta are updated locally below by scaling down
         * by 16 to come to 0-2 range.
         */
        let mut alpha = self.config.alpha;
        let mut beta = self.config.beta;

        /* We scale alpha and beta differently depending on how heavy the
         * congestion is. Please see RFC 8033 for details.
         */
        if self.prob < PIEQueueConfig::MAX_PROB / 10 {
            alpha >>= 1;
            beta >>= 1;

            let mut power = 100;
            while (self.prob < PIEQueueConfig::MAX_PROB / power) && power <= 1000000 {
                alpha >>= 2;
                beta >>= 2;
                power *= 10;
            }
        }

        /* alpha and beta should be between 0 and 32, in multiples of 1/16 */
        let mut delta =
            alpha as i64 * ((qdelay.as_nanos() as i64 - self.config.target.as_nanos() as i64) >> 6);
        delta += beta as i64 * ((qdelay.as_nanos() as i64 - qdelay_old.as_nanos() as i64) >> 6);

        /* to ensure we increase probability in steps of no more than 2% */
        if (delta > (PIEQueueConfig::MAX_PROB / (100 / 2)) as i64)
            && self.prob >= PIEQueueConfig::MAX_PROB / 10
        {
            delta = ((PIEQueueConfig::MAX_PROB / 100) * 2) as i64;
        }

        /* Non-linear drop:
         * Tune drop probability to increase quickly for high delays(>= 250ms)
         * 250ms is derived through experiments and provides error protection
         */

        if qdelay > Duration::from_millis(250) {
            delta += (PIEQueueConfig::MAX_PROB / (100 / 2)) as i64;
        }

        let mut new_prob = match self.prob as i128 + delta as i128 {
            r if r > PIEQueueConfig::MAX_PROB as i128 => {
                /* Prevent normalization error. If probability is at
                 * maximum value already, we normalize it here, and
                 * skip the check to do a non-linear drop in the next
                 * section.
                 */
                update_prob = false;
                PIEQueueConfig::MAX_PROB
            }
            r if r < 0 => 0,
            r => r as u64,
        };

        /* Non-linear drop in probability: Reduce drop probability quickly if
         * delay is 0 for 2 consecutive Tupdate periods.
         */
        if qdelay.as_nanos() == 0 && qdelay_old.as_nanos() == 0 && update_prob {
            /* Reduce drop probability to 98.4% */
            new_prob -= new_prob / 64;
        }

        self.qdelay = qdelay;
        self.backlog_old = self.bytes_in_q;

        /* We restart the measurement cycle if the following conditions are met
         * 1. If the delay has been low for 2 consecutive Tupdate periods
         * 2. Calculated drop probability is zero
         * 3. If average dq_rate_estimator is enabled, we have at least one
         *    estimate for the avg_dq_rate ie., is a non-zero value
         */
        if self.qdelay < self.config.target / 2
            && self.qdelay_old < self.config.target / 2
            && new_prob == 0
            && (!self.config.dq_rate_estimator || self.avg_dq_rate > 0)
        {
            self.burst_time = Duration::from_millis(150);
            self.dq_timestamp = Instant::now();
            self.accu_prob = 0;
            self.dq_count = None;
            self.avg_dq_rate = 0;
        }

        if !self.config.dq_rate_estimator {
            self.qdelay_old = qdelay;
        }

        new_prob
    }
}

impl<P> PacketQueue<P> for PIEQueue<P>
where
    P: Packet,
{
    type Config = PIEQueueConfig;
    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn enqueue(&mut self, packet: P) {
        trace!("PIE Enqueued Packet: {}", packet.desc());
        self.update_prob();
        // Check if overlimit
        if self
            .config
            .packet_limit
            .map_or(false, |limit| self.queue.len() >= limit)
            || self.config.byte_limit.map_or(false, |limit| {
                self.bytes_in_q + packet.l3_length() + self.config.bw_type.extra_length() > limit
            })
        {
            // Over limit, can only drop
            self.accu_prob = 0;
            // Drop the packet
            return;
        }

        let mut control_buffer = PIEPacketControlBuffer::new(packet);

        if self.drop_early(control_buffer.packet.l3_length() as u64) {
            // Need drop, decide whether actual drop or just mark
            if control_buffer.ect != Ipv4Ecn::ZERO && self.prob <= PIEQueueConfig::MAX_PROB / 10 {
                // Set ce as drop
                control_buffer.set_ce();
            } else {
                // Drop the packet
                return;
            }
        }

        if !self.config.dq_rate_estimator {
            control_buffer.packet.set_timestamp(Instant::now());
        }

        self.bytes_in_q += control_buffer.packet.l3_length() + self.config.bw_type.extra_length();
        self.packets_in_q += 1;
        self.queue.push_back(control_buffer);
    }

    fn dequeue(&mut self) -> Option<P> {
        let dequeued_packet_cb = match self.queue.pop_front() {
            None => return None,
            Some(packet) => packet,
        };

        trace!("Dequeued packet: {}", dequeued_packet_cb.packet.desc());
        self.packets_in_q -= 1;
        self.bytes_in_q -=
            dequeued_packet_cb.packet.l3_length() + self.config.bw_type.extra_length();

        let now = Instant::now();
        let dtime: Duration;

        /* If dq_rate_estimator is disabled, calculate qdelay using the
         * packet timestamp.
         */
        if !self.config.dq_rate_estimator {
            self.qdelay = now - dequeued_packet_cb.packet.get_timestamp();

            dtime = now - self.dq_timestamp;

            self.dq_timestamp = now;

            if self.bytes_in_q == 0 {
                self.qdelay = Duration::ZERO;
            }

            if dtime.is_zero() {
                return Some(dequeued_packet_cb.packet);
            }

            // Do burst allowance reduction
            if self.burst_time > Duration::ZERO {
                if self.burst_time > dtime {
                    self.burst_time -= dtime;
                } else {
                    self.burst_time = Duration::ZERO;
                }
            }
            return Some(dequeued_packet_cb.packet);
        }

        /* If current queue is about 10 packets or more and is not conducting measurment
         * we have enough packets to calculate the drain rate. Save
         * current time as dq_tstamp and start measurement cycle.
         */
        if self.bytes_in_q >= PIEQueueConfig::QUEUE_THRESHOLD as usize && self.dq_count.is_none() {
            self.dq_timestamp = Instant::now();
            self.dq_count = Some(0);
        }

        /* Calculate the average drain rate from this value. If queue length
         * has receded to a small value viz., <= QUEUE_THRESHOLD bytes, reset
         * in measurment to false as we don't have enough packets to calculate the
         * drain rate anymore. The following if block is entered only when we
         * have a substantial queue built up (QUEUE_THRESHOLD bytes or more)
         * and we calculate the drain rate for the threshold here.  dq_count is
         * in bytes, hence rate is in
         * bytes/psched_time.
         */
        if let Some(mut dq_count) = self.dq_count {
            dq_count += dequeued_packet_cb.packet.l3_length() as u64;
            self.dq_count = Some(dq_count);

            if dq_count >= PIEQueueConfig::QUEUE_THRESHOLD as u64 {
                let count = dq_count << PIEQueueConfig::PIE_SCALE;

                dtime = now - self.dq_timestamp;

                if dtime == Duration::ZERO {
                    return Some(dequeued_packet_cb.packet);
                }

                // dtime.as_nanos() >> 6 is in unit of the linux 'tick'
                let avg_rate = count as u128 / (dtime.as_nanos() >> 6);

                if self.avg_dq_rate == 0 {
                    self.avg_dq_rate = avg_rate as u32;
                } else {
                    self.avg_dq_rate =
                        (self.avg_dq_rate - (self.avg_dq_rate >> 3)) + (avg_rate >> 3) as u32;
                }

                /* If the queue has receded below the threshold, we hold
                 * on to the last drain rate calculated, else we reset
                 * dq_count to 0 to re-enter the if block when the next
                 * packet is dequeued
                 */
                if self.bytes_in_q < PIEQueueConfig::QUEUE_THRESHOLD as usize {
                    self.dq_count = None;
                } else {
                    self.dq_count = Some(0);
                    self.dq_timestamp = Instant::now();
                }

                // Do burst allowance reduction
                if self.burst_time > Duration::ZERO {
                    if self.burst_time > dtime {
                        self.burst_time -= dtime;
                    } else {
                        self.burst_time = Duration::ZERO;
                    }
                }
            }
        }
        Some(dequeued_packet_cb.packet)
    }
}
