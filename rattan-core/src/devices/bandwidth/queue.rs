use crate::devices::Packet;
use etherparse::Ipv4Ecn;
use rand::{rngs::StdRng, RngCore, SeedableRng};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
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
        let mut credit_change: i32 = 0;

        loop {
            let packet_cb = self.dequeue_packet(&mut credit_change);
            match packet_cb {
                Some(mut packet_cb) => {
                    if !self.config.drop_early && self.must_drop(&mut packet_cb)
                        || (packet_cb.in_l_queue() && self.do_step_aqm(&mut packet_cb, now))
                    {
                        self.defferred_dropped_cnt += 1;
                        self.defferred_dropped_len += packet_cb
                            .packet
                            .ip_hdr()
                            .expect("Cannot find ip header")
                            .payload_len()
                            .expect("Parse total length error")
                            as u32;

                        // Drop the packet
                        continue;
                    }
                    self.c_protection_credit += credit_change;
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

    /* Never drop packets  */
    fn must_drop(&mut self, packet_control_buffer: &mut DualPI2PacketControlBuffer<P>) -> bool {
        if self.bytes_in_c < 2 * self.config.mtu as usize {
            return false;
        }

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

    fn dequeue_packet(&mut self, credit_change: &mut i32) -> Option<DualPI2PacketControlBuffer<P>> {
        /* Prioritize dequeing from the L queue. Only protection credit is > 0
        we try to give out c queue packet. If C queue has no packet when it is
        given the chance the dequeue, we still try to dequeue from L; similarly,
        if there is no packet to dequeue in L queue when it is given the chance
        we will try to dequeue from C queue */
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
                *credit_change = self.config.c_protection_wc as i32;
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
                *credit_change = -(self.c_protection_wl as i32);
            }

            self.packets_in_c -= 1;
            self.bytes_in_c -= dequeued_packet.packet.l3_length();
            dequeued_packet
        } else {
            self.reset_c_protection();
            return None;
        };
        // This is not safe only in theory. l3_length gives usize, which may
        // overflow the i32. However, I fail to come up with a scenario where the
        // packet size will be larger then 2^31 - 1, thus this in practical should
        // never cause any problem.
        *credit_change *= packet.packet.l3_length() as i32;
        Some(packet)
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
        let new_prob = self.pi2_prob as i64 + Self::scale_delta(delta as u64);
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

    fn scale_delta(delta: u64) -> i64 {
        (delta / (1 << DualPI2QueueConfig::ALPHA_BETA_GRANULARITY)) as i64
    }
}
