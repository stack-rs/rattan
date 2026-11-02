//! DualPI2 (Dual-Queue Coupled AQM with PI2) queue, for L4S.
//!
//! DualPI2 keeps two FIFOs behind one bottleneck:
//!
//! - the **L-queue** for L4S traffic (scalable congestion controls such as TCP
//!   Prague), identified by the ECN field (ECT(1) or CE by default), and
//! - the **C-queue** for Classic traffic (Reno, Cubic, ...).
//!
//! A single PI controller computes a base probability `p'` from the larger of
//! the two head-of-line queuing delays.  Classic packets are dropped (or
//! marked, if ECN-capable) with probability `p_C = p'²`, while L4S packets are
//! marked with the *coupled* probability `p_CL = k·p'`.  L4S packets are also
//! marked by a shallow *step* AQM on their own sojourn time, so the L-queue
//! stays around a millisecond deep.  A weighted round-robin scheduler with a
//! credit counter gives the L-queue priority while keeping the C-queue from
//! starving.
//!
//! # References
//!
//! - RFC 9332 (Dual-Queue Coupled AQM):
//!   <https://www.rfc-editor.org/info/rfc9332>
//! - RFC 9331 (ECN Protocol for L4S):
//!   <https://www.rfc-editor.org/info/rfc9331>
//! - Linux kernel DualPI2:
//!   [`net/sched/sch_dualpi2.c`](https://github.com/torvalds/linux/blob/master/net/sched/sch_dualpi2.c)
//!   (referenced as "kernel" throughout this documentation)
//! - Albisser et al., *DUALPI2 - Low Latency, Low Loss and Scalable Throughput
//!   (L4S) AQM*, Netdev 0x13, 2019.  The 2019 version scheduled the two queues
//!   with a time-shifted FIFO; this implementation follows the current kernel
//!   and RFC 9332, which use the WRR credit scheduler described below.
//!
//! # Algorithm (as in the kernel)
//!
//! **Enqueue.**  The packet is classified (L4S if `ECN & ecn_mask != 0`), checked
//! against the hard limits, optionally subjected to the PI2 decision
//! (`drop_early`), and appended to its queue.  An L4S packet remembers whether
//! the step AQM applies to it (`L-queue length >= min_qlen_step` at enqueue).
//!
//! **Dequeue.**  The scheduler picks the L-queue if it is non-empty and either the
//! C-queue is empty or the credit is `<= 0`; otherwise it picks the C-queue.  The
//! packet then goes through, in order:
//!
//! 1. the PI2 decision (unless `drop_early`), which is skipped entirely while the
//!    remaining backlog is below two MTUs:
//!    - Classic: with probability `p'²`, mark if ECN-capable and not
//!      overloaded, otherwise drop;
//!    - L4S, not overloaded (`k·p' <= 1`): mark with probability `k·p'`;
//!    - L4S, overloaded: drop with probability `p'²` if `drop_overload`,
//!      otherwise (or if not dropped) mark;
//! 2. for L4S packets, the step AQM: mark if the sojourn time (or the
//!    L-queue length) exceeds `step_threshold`.
//!
//! A dropped packet is discarded and the scheduler tries again.  Only a packet
//! that is actually sent moves the WRR credit: `+wc·size` for an L4S packet sent
//! while the C-queue is backlogged, `-wl·size` for a Classic packet sent while
//! the L-queue is backlogged.  The credit is reset to `mtu·(wc - wl)` whenever
//! both queues are empty, so the L-queue goes first after an idle period.
//!
//! **Probability update.**  Every `tupdate`:
//!
//! ```text
//! qdelay = max(head-of-line delay of L-queue, head-of-line delay of C-queue)
//! p'    += alpha·(qdelay - target) + beta·(qdelay - qdelay_old)
//! p'     = clamp(p', 0, 1)            (and p' <= 1/k unless drop_overload)
//! ```
//!
//! # Differences from the Linux kernel implementation
//!
//! ## 1. Probability update: catch-up grid instead of a timer
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | trigger | `hrtimer` firing every `tupdate` | catch-up at the start of every `enqueue()` / `dequeue_at()` |
//! | delays used | head-of-line delays at the moment the timer fires | head-of-line delays at each grid instant |
//! | grid origin | qdisc creation | first packet seen by the queue |
//! | runs while idle? | yes | yes (the missed grid instants are replayed) |
//!
//! The simulator runs on *logical* time, so a wall-clock timer cannot fire at
//! the right logical instant.  Instead, before handling an event at logical time
//! `now`, every grid instant `t <= now` that has not been processed yet is
//! replayed in order, computing the head-of-line delays *as of `t`*.  The queue
//! contents only change at events, which are processed in timestamp order, so
//! the queue seen at `t` is exactly the queue the kernel timer would have seen.
//! The replay is therefore equivalent to a perfectly punctual timer (unlike
//! [`PieQueue`](super::PieQueue), which samples the state at the current event).
//!
//! Long idle periods are skipped in O(1) once the controller has reached its
//! idle fixed point (both queues empty, `p' = 0`, `qdelay_old = 0`), since every
//! further update is then a no-op.
//!
//! ## 2. Floating point instead of fixed point
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | `p'` | `u32` scaled by `U32_MAX` | `f64` in `[0, 1]` |
//! | `alpha`, `beta` | integers in units of 1/256 Hz; defaults 41 and 819 | `f64` in Hz; defaults 0.16 and 3.2 |
//! | random draw | `get_random_u32() <= prob` | `uniform[0, 1) < p` |
//!
//! The kernel defaults are `41/256 ≈ 0.1602` and `819/256 ≈ 3.1992`; the defaults
//! here are the RFC 9332 values they approximate.  `alpha` and `beta` are gains
//! per update interval: `p'` changes by `alpha·(qdelay - target)` per `tupdate`,
//! with delays in seconds, exactly as in the kernel.
//!
//! ## 3. Classification
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | ECN-based (`ecn_mask`) | yes | yes |
//! | `skb->priority` / tc filters | yes, may select any class | not supported |
//! | `LLLL` class (L-queue, never marked or dropped) | yes, via priority or filters | not supported |
//!
//! Only the ECN field selects the queue.  Since `ecn_mask` is never zero, a
//! Not-ECT packet can never enter the L-queue, so the kernel's code paths that
//! drop Not-ECT packets found in the L-queue are unreachable here and omitted.
//! Packets that are not IPv4/IPv6 (see [`crate::cells::ecn`]) are Classic and
//! Not-ECT.
//!
//! ## 4. Limits and MTU
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | packet limit | `limit`, default 10000 | `packet_limit`, default `Some(10000)` |
//! | byte limit | `memory_limit` on `skb->truesize`, default `2·limit·MTU` | `byte_limit` on packet sizes, default `None` |
//! | MTU | device MTU + L2 header (`psched_mtu()`) | `mtu` + [`BwType`] overhead |
//!
//! The MTU is used for the "never mark or drop below two MTUs of backlog" rule
//! and for the initial WRR credit.  Packet sizes are measured as everywhere in
//! Rattan: L3 length plus the [`BwType`] overhead.
//!
//! ## 5. Other
//!
//! - **GSO splitting** (`split_gso`) does not apply: Rattan handles one frame at
//!   a time (offloads are disabled on its veth pairs).
//! - **Timestamps**: the enqueue time is the packet's logical timestamp, and the
//!   sojourn time is `dequeue_at(timestamp) - packet.get_timestamp()`.
//! - **RNG**: a seedable [`StdRng`] makes runs reproducible.
//! - **Statistics** (`ecn_mark`, `step_marks`, ...) are not exported; the
//!   probability and delays are logged at `trace` level on each update.

use std::collections::VecDeque;

use rand::{rngs::StdRng, RngExt, SeedableRng};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, Instant};
use tracing::{debug, trace, warn};

#[cfg(feature = "serde")]
use super::serde_default;
use super::{BwType, PacketQueue};
use crate::cells::Packet;

/// Weights of the WRR scheduler are expressed in percent: `wc + wl = 100`.
const MAX_WC: u8 = 100;

/// Which ECN codepoints are classified into the L-queue.
///
/// A packet goes to the L-queue if `ECN & mask != 0`, so CE packets always do.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DualPi2EcnMask {
    /// ECT(1) and CE: the L4S identifier of RFC 9331 (kernel `l4s_ect`).
    #[default]
    L4sEct,
    /// ECT(0) and CE (kernel `cla_ect`).
    ClaEct,
    /// ECT(0), ECT(1) and CE, e.g. for DCTCP, which uses ECT(0) (kernel `any_ect`).
    AnyEct,
}

impl DualPi2EcnMask {
    fn bits(self) -> u8 {
        match self {
            DualPi2EcnMask::L4sEct => 0b01,
            DualPi2EcnMask::ClaEct => 0b10,
            DualPi2EcnMask::AnyEct => 0b11,
        }
    }
}

/// Threshold of the step AQM applied to L4S packets.
///
/// In TOML/JSON, a duration string (e.g. `"1ms"`) selects [`Time`](Self::Time)
/// and an integer (e.g. `5`) selects [`Packets`](Self::Packets).
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(untagged))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DualPi2StepThreshold {
    /// Mark when the L-queue holds more than this many packets after the
    /// packet has been dequeued (kernel `step_thresh_pkts`).
    Packets(u32),
    /// Mark when the packet's sojourn time exceeds this duration (kernel
    /// `step_thresh_us`).
    Time(#[cfg_attr(feature = "serde", serde(with = "crate::utils::serde::duration"))] Duration),
}

/// Configuration for a [`DualPi2Queue`].
///
/// All defaults follow the Linux kernel (`dualpi2_reset_default()`).
///
/// | Field | Kernel (`tc qdisc ... dualpi2`) | Default |
/// |-------|---------------------------------|---------|
/// | `packet_limit` | `limit` | `Some(10000)` |
/// | `byte_limit` | *(`memory_limit` counts `skb->truesize`)* | `None` |
/// | `target` | `target` | 15 ms |
/// | `tupdate` | `tupdate` | 16 ms |
/// | `alpha` | `alpha` | 0.16 |
/// | `beta` | `beta` | 3.2 |
/// | `step_threshold` | `step_thresh` | 1 ms |
/// | `min_qlen_step` | `min_qlen_step` | 0 |
/// | `coupling_factor` | `coupling_factor` | 2.0 |
/// | `c_protection` | `c_protection` | 10 (%) |
/// | `ecn_mask` | `l4s_ect` / `cla_ect` / `any_ect` | `L4sEct` |
/// | `drop_early` | `drop_enqueue` / `drop_dequeue` | `false` (drop at dequeue) |
/// | `drop_overload` | `drop_overload` / `overflow` | `true` |
/// | `mtu` | *(device MTU)* | 1500 |
/// | `bw_type` | — | `NetworkLayer` |
/// | `seed` | — | 42 |
///
/// ```no_run
/// # use rattan_core::cells::bandwidth::queue::{DualPi2QueueConfig, DualPi2StepThreshold};
/// # use std::time::Duration;
/// let cfg = DualPi2QueueConfig {
///     step_threshold: DualPi2StepThreshold::Time(Duration::from_millis(3)),
///     ..Default::default()
/// };
/// ```
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(default))]
#[derive(Debug, Clone, PartialEq)]
pub struct DualPi2QueueConfig {
    /// Hard limit on the total number of packets in both queues.  `None` means
    /// no limit; `Some(0)` makes the queue a zero-buffer.
    ///
    /// Default: `Some(10000)`, as in the kernel.
    pub packet_limit: Option<usize>,

    /// Hard limit on the total bytes in both queues (L3 length + L2 overhead
    /// from [`bw_type`](Self::bw_type)).  `None` means no limit.
    ///
    /// Default: `None`.
    pub byte_limit: Option<usize>,

    /// Target queuing delay of the PI controller (for the Classic queue).
    ///
    /// Default: 15 ms.
    #[cfg_attr(feature = "serde", serde(with = "crate::utils::serde::duration"))]
    pub target: Duration,

    /// Interval between probability updates.  Must be `> 0`.
    ///
    /// Default: 16 ms.
    #[cfg_attr(feature = "serde", serde(with = "crate::utils::serde::duration"))]
    pub tupdate: Duration,

    /// Integral gain, in Hz.  Must be `> 0` and finite.
    ///
    /// Default: `0.16`.
    pub alpha: f64,

    /// Proportional gain, in Hz.  Must be `> 0` and finite.
    ///
    /// Default: `3.2`.
    pub beta: f64,

    /// Threshold of the step AQM on the L-queue.
    ///
    /// Default: `Time(1 ms)`.
    pub step_threshold: DualPi2StepThreshold,

    /// The step AQM only applies to an L4S packet if the L-queue already held at
    /// least this many packets when it was enqueued.
    ///
    /// Default: `0` (always apply).
    pub min_qlen_step: usize,

    /// Coupling factor `k` between the base probability and the L4S marking
    /// probability (`p_CL = k·p'`).  Must be `> 0` and finite.
    ///
    /// Default: `2.0` (window fairness between L4S and Classic flows with
    /// equal RTTs).
    pub coupling_factor: f64,

    /// Weight of the C-queue in the WRR scheduler, in percent (`0..=100`).
    /// The L-queue gets the rest.
    ///
    /// Default: `10`.
    pub c_protection: u8,

    /// ECN codepoints classified into the L-queue.
    ///
    /// Default: [`DualPi2EcnMask::L4sEct`].
    pub ecn_mask: DualPi2EcnMask,

    /// Apply the PI2 decision at enqueue (`true`) instead of dequeue (`false`).
    ///
    /// Default: `false`.
    pub drop_early: bool,

    /// When overloaded (`k·p' > 1`), drop L4S packets with probability `p'²`
    /// (`true`), or keep marking them and let the queue grow up to the hard
    /// limit (`false`).
    ///
    /// Default: `true`.
    pub drop_overload: bool,

    /// MTU in bytes, at L3.  The [`bw_type`](Self::bw_type) overhead is added.
    /// Must be `> 0`.
    ///
    /// Default: `1500`.
    pub mtu: usize,

    /// L2 overhead mode for byte accounting.
    ///
    /// Default: [`BwType::NetworkLayer`] (no overhead).
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "serde_default")
    )]
    pub bw_type: BwType,

    /// Seed for the deterministic RNG used in mark and drop decisions.
    ///
    /// Default: `42`.
    pub seed: u64,
}

impl Default for DualPi2QueueConfig {
    fn default() -> Self {
        Self {
            packet_limit: Some(10000),
            byte_limit: None,
            target: Duration::from_millis(15),
            tupdate: Duration::from_millis(16),
            alpha: 0.16,
            beta: 3.2,
            step_threshold: DualPi2StepThreshold::Time(Duration::from_millis(1)),
            min_qlen_step: 0,
            coupling_factor: 2.0,
            c_protection: 10,
            ecn_mask: DualPi2EcnMask::default(),
            drop_early: false,
            drop_overload: true,
            mtu: 1500,
            bw_type: BwType::default(),
            seed: 42,
        }
    }
}

impl DualPi2QueueConfig {
    fn validate(&self) -> Result<(), &'static str> {
        if self.tupdate.is_zero() {
            return Err("DualPi2QueueConfig: tupdate must be > 0");
        }
        if self.alpha <= 0.0 || !self.alpha.is_finite() {
            return Err("DualPi2QueueConfig: alpha must be > 0 and finite");
        }
        if self.beta <= 0.0 || !self.beta.is_finite() {
            return Err("DualPi2QueueConfig: beta must be > 0 and finite");
        }
        if self.coupling_factor <= 0.0 || !self.coupling_factor.is_finite() {
            return Err("DualPi2QueueConfig: coupling_factor must be > 0 and finite");
        }
        if self.c_protection > MAX_WC {
            return Err("DualPi2QueueConfig: c_protection must be <= 100");
        }
        if self.mtu == 0 {
            return Err("DualPi2QueueConfig: mtu must be > 0");
        }
        Ok(())
    }
}

impl<P: Packet> TryFrom<DualPi2QueueConfig> for DualPi2Queue<P> {
    type Error = &'static str;

    fn try_from(config: DualPi2QueueConfig) -> Result<Self, Self::Error> {
        DualPi2Queue::new(config)
    }
}

#[derive(Debug)]
struct L4sPacket<P> {
    packet: P,
    /// Whether the step AQM applies to this packet, decided at enqueue.
    apply_step: bool,
}

#[derive(Debug)]
pub struct DualPi2Queue<P> {
    config: DualPi2QueueConfig,
    l_queue: VecDeque<L4sPacket<P>>,
    c_queue: VecDeque<P>,
    /// Total bytes in both queues.
    now_bytes: usize,
    /// Base probability `p'`.
    prob: f64,
    /// Queuing delay at the last probability update.
    last_qdelay: Duration,
    /// Next instant of the probability update grid, set by the first event.
    next_update: Option<Instant>,
    /// WRR credit: the L-queue is served while it is `<= 0`.
    c_protection_credit: i64,
    rng: StdRng,
}

impl<P> Default for DualPi2Queue<P>
where
    P: Packet,
{
    fn default() -> Self {
        Self::new(DualPi2QueueConfig::default())
            .expect("DualPi2QueueConfig::default() should never fail validation")
    }
}

impl<P> DualPi2Queue<P>
where
    P: Packet,
{
    #[inline(always)]
    fn mtu(&self) -> usize {
        self.config.mtu + self.get_extra_length()
    }

    fn reset_c_protection(&mut self) {
        let wc = i64::from(self.config.c_protection);
        let wl = i64::from(MAX_WC) - wc;
        self.c_protection_credit = self.mtu() as i64 * (wc - wl);
    }

    /// Whether the next dequeue should be served from the L-queue.
    fn serve_l_queue(&self) -> bool {
        !self.l_queue.is_empty() && (self.c_queue.is_empty() || self.c_protection_credit <= 0)
    }

    fn is_over_limit(&self) -> bool {
        self.config
            .packet_limit
            .is_some_and(|limit| self.length() > limit)
            || self
                .config
                .byte_limit
                .is_some_and(|limit| self.now_bytes > limit)
    }

    /// Replays every probability update scheduled at or before `now`.
    fn update_probability_until(&mut self, now: Instant) {
        let tupdate = self.config.tupdate;
        let Some(mut next_update) = self.next_update else {
            self.next_update = Some(now + tupdate);
            return;
        };
        while next_update <= now {
            if self.is_empty() && self.prob == 0.0 && self.last_qdelay.is_zero() {
                // Idle fixed point: all remaining updates up to `now` are no-ops.
                let skipped = (now - next_update).as_nanos() / tupdate.as_nanos() + 1;
                next_update += Duration::from_nanos((skipped * tupdate.as_nanos()) as u64);
                break;
            }
            self.update_probability(next_update);
            next_update += tupdate;
        }
        self.next_update = Some(next_update);
    }

    fn update_probability(&mut self, now: Instant) {
        let head_delay = |timestamp: Option<Instant>| {
            timestamp.map_or(Duration::ZERO, |ts| now.saturating_duration_since(ts))
        };
        let qdelay_l = head_delay(self.l_queue.front().map(|p| p.packet.get_timestamp()));
        let qdelay_c = head_delay(self.c_queue.front().map(|p| p.get_timestamp()));
        let qdelay = qdelay_l.max(qdelay_c);

        let qdelay_secs = qdelay.as_secs_f64();
        let delta = self.config.alpha * (qdelay_secs - self.config.target.as_secs_f64())
            + self.config.beta * (qdelay_secs - self.last_qdelay.as_secs_f64());
        self.last_qdelay = qdelay;

        let mut prob = (self.prob + delta).clamp(0.0, 1.0);
        if !self.config.drop_overload {
            // Cap the L4S marking probability at 100% to keep window fairness.
            prob = prob.min(1.0 / self.config.coupling_factor);
        }
        self.prob = prob;
        trace!(
            prob,
            ?qdelay_l,
            ?qdelay_c,
            "DualPi2Queue probability updated"
        );
    }

    #[inline(always)]
    fn roll(&mut self, prob: f64) -> bool {
        self.rng.random_range(0.0..1.0) < prob
    }

    /// The PI2 decision.  Returns `true` if the packet must be dropped; may mark
    /// the packet with CE instead.
    ///
    /// Must be called with `now_bytes` not including `packet`.
    fn must_drop(&mut self, packet: &mut P, is_l4s: bool) -> bool {
        // Never mark or drop if the standing queue is less than 2 MTUs.
        if self.now_bytes < 2 * self.mtu() {
            return false;
        }
        let prob = self.prob;
        let l4s_prob = prob * self.config.coupling_factor;
        let overload = l4s_prob > 1.0;

        if !is_l4s {
            // Classic: p_C = p'², drop instead of marking when overloaded.
            return self.roll(prob) && self.roll(prob) && (overload || !packet.set_ce());
        }
        if overload {
            if self.config.drop_overload && self.roll(prob) && self.roll(prob) {
                return true;
            }
            packet.set_ce();
        } else if self.roll(l4s_prob) {
            packet.set_ce();
        }
        false
    }

    fn step_threshold_exceeded(&self, packet: &P, now: Instant) -> bool {
        match self.config.step_threshold {
            DualPi2StepThreshold::Packets(threshold) => self.l_queue.len() > threshold as usize,
            DualPi2StepThreshold::Time(threshold) => {
                now.saturating_duration_since(packet.get_timestamp()) > threshold
            }
        }
    }
}

impl<P> PacketQueue<P> for DualPi2Queue<P>
where
    P: Packet,
{
    type Config = DualPi2QueueConfig;

    fn new(config: DualPi2QueueConfig) -> Result<Self, &'static str> {
        config.validate()?;
        debug!(?config, "New DualPi2Queue");
        let seed = config.seed;
        let mut queue = Self {
            config,
            l_queue: VecDeque::new(),
            c_queue: VecDeque::new(),
            now_bytes: 0,
            prob: 0.0,
            last_qdelay: Duration::ZERO,
            next_update: None,
            c_protection_credit: 0,
            rng: StdRng::seed_from_u64(seed),
        };
        queue.reset_c_protection();
        Ok(queue)
    }

    fn configure(&mut self, config: Self::Config) {
        if let Err(e) = config.validate() {
            warn!("DualPi2Queue: discard invalid configure: {}", e);
            return;
        }
        if config.seed != self.config.seed {
            self.rng = StdRng::seed_from_u64(config.seed);
        }
        let credit_changed = config.c_protection != self.config.c_protection
            || config.mtu != self.config.mtu
            || config.bw_type != self.config.bw_type;
        self.config = config;
        if credit_changed {
            self.reset_c_protection();
        }
        // As in the kernel, shrinking the limits drops packets from the head of
        // the C-queue first, then from the head of the L-queue.
        while self.is_over_limit() {
            let packet = match self.c_queue.pop_front() {
                Some(packet) => packet,
                None => match self.l_queue.pop_front() {
                    Some(l4s_packet) => l4s_packet.packet,
                    None => break,
                },
            };
            self.now_bytes -= self.get_packet_size(&packet);
        }
    }

    fn is_zero_buffer(&self) -> bool {
        self.config.packet_limit.is_some_and(|limit| limit == 0)
            || self.config.byte_limit.is_some_and(|limit| limit == 0)
    }

    fn enqueue(&mut self, mut packet: P) {
        self.update_probability_until(packet.get_timestamp());

        let ecn_mask = self.config.ecn_mask.bits();
        let is_l4s = packet.ecn().is_some_and(|ecn| ecn.value() & ecn_mask != 0);

        let packet_size = self.get_packet_size(&packet);
        let below_hard_limit = self
            .config
            .packet_limit
            .is_none_or(|limit| self.length() < limit)
            && self
                .config
                .byte_limit
                .is_none_or(|limit| self.now_bytes + packet_size <= limit);
        if !below_hard_limit {
            #[cfg(test)]
            trace!(
                queue_len = self.length(),
                now_bytes = self.now_bytes,
                "Drop packet(l3_len: {}, extra_len: {}) due to hard limit",
                packet.l3_length(),
                self.get_extra_length()
            );
            return;
        }

        if self.config.drop_early && self.must_drop(&mut packet, is_l4s) {
            #[cfg(test)]
            trace!(
                prob = self.prob,
                is_l4s,
                "Drop packet(l3_len: {}, extra_len: {}) at enqueue due to PI2",
                packet.l3_length(),
                self.get_extra_length()
            );
            return;
        }

        self.now_bytes += packet_size;
        if is_l4s {
            let apply_step = self.l_queue.len() >= self.config.min_qlen_step;
            self.l_queue.push_back(L4sPacket { packet, apply_step });
        } else {
            self.c_queue.push_back(packet);
        }
    }

    fn dequeue_at(&mut self, timestamp: Instant) -> Option<P> {
        self.update_probability_until(timestamp);

        loop {
            let is_l4s = self.serve_l_queue();
            let (mut packet, apply_step) = if is_l4s {
                let L4sPacket { packet, apply_step } = self.l_queue.pop_front()?;
                (packet, apply_step)
            } else if let Some(packet) = self.c_queue.pop_front() {
                (packet, false)
            } else {
                self.reset_c_protection();
                return None;
            };
            let packet_size = self.get_packet_size(&packet);
            self.now_bytes -= packet_size;

            // Only applied if the packet is actually sent.
            let credit_change = if is_l4s && !self.c_queue.is_empty() {
                i64::from(self.config.c_protection) * packet_size as i64
            } else if !is_l4s && !self.l_queue.is_empty() {
                -i64::from(MAX_WC - self.config.c_protection) * packet_size as i64
            } else {
                0
            };

            if !self.config.drop_early && self.must_drop(&mut packet, is_l4s) {
                #[cfg(test)]
                trace!(
                    prob = self.prob,
                    is_l4s,
                    "Drop packet(l3_len: {}, extra_len: {}) at dequeue due to PI2",
                    packet.l3_length(),
                    self.get_extra_length()
                );
                continue;
            }

            // Step AQM.  L4S packets are always ECN-capable, so they are never
            // dropped here.
            if apply_step && self.step_threshold_exceeded(&packet, timestamp) {
                packet.set_ce();
            }

            self.c_protection_credit += credit_change;
            return Some(packet);
        }
    }

    fn is_empty(&self) -> bool {
        self.l_queue.is_empty() && self.c_queue.is_empty()
    }

    #[inline(always)]
    fn get_extra_length(&self) -> usize {
        self.config.bw_type.extra_length()
    }

    fn get_front_size(&self) -> Option<usize> {
        if self.serve_l_queue() {
            self.l_queue
                .front()
                .map(|p| self.get_packet_size(&p.packet))
        } else {
            self.c_queue.front().map(|p| self.get_packet_size(p))
        }
    }

    fn length(&self) -> usize {
        self.l_queue.len() + self.c_queue.len()
    }
}

#[cfg(test)]
mod tests {
    use etherparse::{IpNumber, Ipv4Header};

    use super::*;
    use crate::cells::{ecn::IpEcn, StdPacket};

    const MS: Duration = Duration::from_millis(1);

    /// An Ethernet II frame carrying IPv4 with the given ECN codepoint and L3 length.
    fn packet(ecn: IpEcn, l3_length: usize, timestamp: Instant) -> StdPacket {
        let mut header = Ipv4Header::new(
            (l3_length - Ipv4Header::MIN_LEN) as u16,
            64,
            IpNumber::UDP,
            [10, 1, 1, 1],
            [10, 2, 1, 1],
        )
        .unwrap();
        header.ecn = ecn;
        header.header_checksum = header.calc_header_checksum();
        let mut buf = vec![0x02, 0, 0, 0, 0, 0x01, 0x02, 0, 0, 0, 0, 0x02, 0x08, 0x00];
        buf.extend_from_slice(&header.to_bytes());
        buf.resize(14 + l3_length, 0);
        StdPacket::with_timestamp(&buf, timestamp)
    }

    fn mtu_packet(ecn: IpEcn, timestamp: Instant) -> StdPacket {
        packet(ecn, 1500, timestamp)
    }

    fn queue(config: DualPi2QueueConfig) -> DualPi2Queue<StdPacket> {
        DualPi2Queue::new(config).unwrap()
    }

    /// A config whose probability never updates during a test and whose step AQM
    /// never fires, so that `prob` can be set by hand.
    fn frozen_config() -> DualPi2QueueConfig {
        DualPi2QueueConfig {
            tupdate: Duration::from_secs(3600),
            step_threshold: DualPi2StepThreshold::Time(Duration::from_secs(3600)),
            packet_limit: None,
            ..Default::default()
        }
    }

    #[test_log::test]
    fn test_dualpi2_classification() {
        use IpEcn::*;
        let cases = [
            (DualPi2EcnMask::L4sEct, [false, true, false, true]),
            (DualPi2EcnMask::ClaEct, [false, false, true, true]),
            (DualPi2EcnMask::AnyEct, [false, true, true, true]),
        ];
        let now = Instant::now();
        for (ecn_mask, expected) in cases {
            for (ecn, expect_l4s) in [NotEct, Ect1, Ect0, CongestionExperienced]
                .into_iter()
                .zip(expected)
            {
                let mut q = queue(DualPi2QueueConfig {
                    ecn_mask,
                    ..Default::default()
                });
                q.enqueue(mtu_packet(ecn, now));
                assert_eq!(
                    q.l_queue.len(),
                    usize::from(expect_l4s),
                    "{ecn_mask:?} {ecn:?}"
                );
                assert_eq!(
                    q.c_queue.len(),
                    usize::from(!expect_l4s),
                    "{ecn_mask:?} {ecn:?}"
                );
            }
        }

        // Non-IP frames are Classic.
        let mut q = queue(Default::default());
        q.enqueue(StdPacket::with_timestamp(&[0u8; 100], now));
        assert_eq!(q.c_queue.len(), 1);
    }

    #[test_log::test]
    fn test_dualpi2_hard_limits() {
        let now = Instant::now();
        assert_eq!(DualPi2QueueConfig::default().packet_limit, Some(10000));

        let mut q = queue(DualPi2QueueConfig {
            packet_limit: Some(3),
            ..Default::default()
        });
        for ecn in [IpEcn::Ect1, IpEcn::NotEct, IpEcn::Ect1, IpEcn::NotEct] {
            q.enqueue(mtu_packet(ecn, now));
        }
        assert_eq!(q.length(), 3, "the limit counts both queues");

        let mut q = queue(DualPi2QueueConfig {
            byte_limit: Some(3000),
            bw_type: BwType::LinkLayer,
            ..Default::default()
        });
        q.enqueue(mtu_packet(IpEcn::NotEct, now));
        assert_eq!(q.now_bytes, 1538);
        q.enqueue(mtu_packet(IpEcn::Ect1, now)); // 1538 + 1538 > 3000
        assert_eq!(q.length(), 1);
        q.enqueue(packet(IpEcn::Ect1, 1462, now)); // 1538 + 1500 > 3000
        assert_eq!(q.length(), 1);
        q.enqueue(packet(IpEcn::Ect1, 1424, now)); // 1538 + 1462 == 3000
        assert_eq!(q.length(), 2);
        assert_eq!(q.now_bytes, 3000);

        for limit in [(Some(0), None), (None, Some(0))] {
            let q = queue(DualPi2QueueConfig {
                packet_limit: limit.0,
                byte_limit: limit.1,
                ..Default::default()
            });
            assert!(q.is_zero_buffer());
        }
        assert!(!queue(Default::default()).is_zero_buffer());
    }

    #[test_log::test]
    fn test_dualpi2_step_marking_by_time() {
        let t0 = Instant::now();
        let mut q = queue(Default::default());
        for _ in 0..3 {
            q.enqueue(mtu_packet(IpEcn::Ect1, t0));
            q.enqueue(mtu_packet(IpEcn::Ect0, t0));
        }
        // Sojourn time 1 ms is not above the 1 ms threshold.
        let p = q.dequeue_at(t0 + MS).unwrap();
        assert_eq!(p.ecn(), Some(IpEcn::Ect1));
        // Above the threshold: L4S packets are marked, Classic ones are not.
        let mut l4s = 0;
        while let Some(p) = q.dequeue_at(t0 + 2 * MS) {
            match p.ecn().unwrap() {
                IpEcn::CongestionExperienced => l4s += 1,
                IpEcn::Ect0 => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(l4s, 2);
        assert_eq!(q.prob, 0.0, "no PI2 marks involved");
    }

    #[test_log::test]
    fn test_dualpi2_step_marking_by_packets() {
        let t0 = Instant::now();
        let mut q = queue(DualPi2QueueConfig {
            step_threshold: DualPi2StepThreshold::Packets(2),
            ..Default::default()
        });
        for _ in 0..5 {
            q.enqueue(mtu_packet(IpEcn::Ect1, t0));
        }
        // L-queue length after each dequeue: 4, 3, 2, 1, 0.
        let marked: Vec<bool> = std::iter::from_fn(|| q.dequeue_at(t0))
            .map(|p| p.ecn() == Some(IpEcn::CongestionExperienced))
            .collect();
        assert_eq!(marked, [true, true, false, false, false]);
    }

    #[test_log::test]
    fn test_dualpi2_min_qlen_step() {
        let t0 = Instant::now();
        let mut q = queue(DualPi2QueueConfig {
            min_qlen_step: 3,
            ..Default::default()
        });
        for _ in 0..5 {
            q.enqueue(mtu_packet(IpEcn::Ect1, t0));
        }
        // L-queue length at enqueue: 0, 1, 2, 3, 4.
        let marked: Vec<bool> = std::iter::from_fn(|| q.dequeue_at(t0 + 10 * MS))
            .map(|p| p.ecn() == Some(IpEcn::CongestionExperienced))
            .collect();
        assert_eq!(marked, [false, false, false, true, true]);
    }

    /// Sends `n` packets through a queue kept 30 MTUs deep, so that the 2-MTU
    /// rule practically never applies even after consecutive drops.
    ///
    /// Returns (dropped, marked); every decision is on a packet that was either
    /// dropped or sent, so the drop rate is `dropped / (dropped + n)`.
    fn run_steady(q: &mut DualPi2Queue<StdPacket>, ecn: IpEcn, n: usize) -> (usize, usize) {
        let now = Instant::now();
        let (mut enqueued, mut sent, mut marked) = (0, 0, 0);
        while sent < n {
            while q.length() < 30 {
                q.enqueue(mtu_packet(ecn, now));
                enqueued += 1;
            }
            let p = q.dequeue_at(now).unwrap();
            sent += 1;
            marked += usize::from(p.ecn() == Some(IpEcn::CongestionExperienced));
        }
        (enqueued - sent - q.length(), marked)
    }

    fn drop_rate(dropped: usize, sent: usize) -> f64 {
        dropped as f64 / (dropped + sent) as f64
    }

    #[test_log::test]
    fn test_dualpi2_coupling() {
        const N: usize = 100_000;
        let p = 0.1;

        // L4S: marked with k·p' = 0.2, never dropped.
        let mut q = queue(frozen_config());
        q.prob = p;
        let (dropped, marked) = run_steady(&mut q, IpEcn::Ect1, N);
        assert_eq!(dropped, 0);
        let rate = marked as f64 / N as f64;
        assert!((rate - 2.0 * p).abs() < 0.01, "L4S mark rate {rate}");

        // ECN-capable Classic: marked with p'² = 0.01, never dropped.
        let mut q = queue(frozen_config());
        q.prob = p;
        let (dropped, marked) = run_steady(&mut q, IpEcn::Ect0, N);
        assert_eq!(dropped, 0);
        let rate = marked as f64 / N as f64;
        assert!((rate - p * p).abs() < 0.002, "Classic mark rate {rate}");

        // Not-ECT Classic: dropped with p'² = 0.01.
        let mut q = queue(frozen_config());
        q.prob = p;
        let (dropped, marked) = run_steady(&mut q, IpEcn::NotEct, N);
        assert_eq!(marked, 0);
        let rate = drop_rate(dropped, N);
        assert!((rate - p * p).abs() < 0.002, "Classic drop rate {rate}");
    }

    #[test_log::test]
    fn test_dualpi2_overload() {
        const N: usize = 20_000;
        let p = 0.6; // k·p' = 1.2 > 1

        // drop_overload: L4S packets are dropped with p'² = 0.36, the rest marked.
        let mut q = queue(frozen_config());
        q.prob = p;
        let (dropped, marked) = run_steady(&mut q, IpEcn::Ect1, N);
        assert_eq!(marked, N, "all surviving L4S packets are marked");
        let rate = drop_rate(dropped, N);
        assert!((rate - p * p).abs() < 0.02, "L4S drop rate {rate}");

        // ECN-capable Classic packets are dropped, not marked, on overload.
        let mut q = queue(frozen_config());
        q.prob = p;
        let (dropped, marked) = run_steady(&mut q, IpEcn::Ect0, N);
        assert_eq!(marked, 0);
        let rate = drop_rate(dropped, N);
        assert!((rate - p * p).abs() < 0.02, "Classic drop rate {rate}");

        // Without drop_overload, L4S packets are all marked and none dropped.
        let mut q = queue(DualPi2QueueConfig {
            drop_overload: false,
            ..frozen_config()
        });
        q.prob = p;
        assert_eq!(run_steady(&mut q, IpEcn::Ect1, N), (0, N));
    }

    #[test_log::test]
    fn test_dualpi2_no_drop_below_two_mtus() {
        let now = Instant::now();
        let mut q = queue(frozen_config());
        q.prob = 1.0;
        // Backlog after dequeue: 1500 + 1499 < 2 MTUs, so nothing is dropped.
        q.enqueue(mtu_packet(IpEcn::NotEct, now));
        q.enqueue(mtu_packet(IpEcn::NotEct, now));
        q.enqueue(packet(IpEcn::NotEct, 1499, now));
        assert_eq!(std::iter::from_fn(|| q.dequeue_at(now)).count(), 3);

        // Backlog after dequeue: 2 MTUs, so the first packet is dropped.
        q.enqueue(mtu_packet(IpEcn::NotEct, now));
        q.enqueue(mtu_packet(IpEcn::NotEct, now));
        q.enqueue(mtu_packet(IpEcn::NotEct, now));
        assert_eq!(std::iter::from_fn(|| q.dequeue_at(now)).count(), 2);
    }

    #[test_log::test]
    fn test_dualpi2_drop_early() {
        let now = Instant::now();
        let mut q = queue(DualPi2QueueConfig {
            drop_early: true,
            ..frozen_config()
        });
        q.prob = 1.0;
        for _ in 0..2 {
            q.enqueue(mtu_packet(IpEcn::NotEct, now));
        }
        assert_eq!(q.length(), 2);
        // Backlog reaches 2 MTUs: dropped at enqueue.
        q.enqueue(mtu_packet(IpEcn::NotEct, now));
        assert_eq!(q.length(), 2);
        // Overloaded L4S packet is dropped at enqueue too.
        q.enqueue(mtu_packet(IpEcn::Ect1, now));
        assert_eq!(q.length(), 2);
        // Not overloaded (k·p' = 1): ECN-capable Classic packets are marked at
        // enqueue with p'² = 0.25.
        q.prob = 0.5;
        let mut marked = 0;
        for _ in 0..1000 {
            q.enqueue(mtu_packet(IpEcn::Ect0, now));
            if let Some(p) = q.c_queue.back() {
                marked += usize::from(p.ecn() == Some(IpEcn::CongestionExperienced));
            }
            while q.length() > 2 {
                q.c_queue.pop_back();
                q.now_bytes -= 1500;
            }
        }
        assert!(
            (200..300).contains(&marked),
            "marked {marked} of 1000 at enqueue"
        );
        // Nothing more happens at dequeue.
        q.prob = 1.0;
        assert_eq!(std::iter::from_fn(|| q.dequeue_at(now)).count(), 2);
    }

    #[test_log::test]
    fn test_dualpi2_wrr_scheduler() {
        let now = Instant::now();
        let mut q = queue(frozen_config());
        for _ in 0..10 {
            q.enqueue(mtu_packet(IpEcn::Ect1, now));
            q.enqueue(mtu_packet(IpEcn::NotEct, now));
        }
        // The credit starts at mtu·(wc - wl) < 0, so the L-queue goes first.
        assert_eq!(q.c_protection_credit, 1500 * (10 - 90));
        let mut classic = 0;
        for _ in 0..1000 {
            let p = q.dequeue_at(now).unwrap();
            let is_l4s = p.ecn() == Some(IpEcn::Ect1);
            classic += usize::from(!is_l4s);
            q.enqueue(mtu_packet(p.ecn().unwrap(), now));
        }
        // With equal sizes, Classic gets wc / (wc + wl) = 10% of the packets.
        assert!(
            (99..=101).contains(&classic),
            "classic share {classic}/1000"
        );

        // With only one queue backlogged, it gets everything and the credit
        // does not move.
        let mut q = queue(frozen_config());
        for _ in 0..10 {
            q.enqueue(mtu_packet(IpEcn::NotEct, now));
        }
        let credit = q.c_protection_credit;
        assert_eq!(std::iter::from_fn(|| q.dequeue_at(now)).count(), 10);
        assert_eq!(q.c_protection_credit, credit);
    }

    #[test_log::test]
    fn test_dualpi2_wrr_credit_ignores_dropped_packets() {
        let now = Instant::now();
        let mut q = queue(DualPi2QueueConfig {
            drop_overload: false,
            ..frozen_config()
        });
        // Overloaded: Classic packets are always dropped, L4S ones always marked.
        q.prob = 1.0;
        for _ in 0..3 {
            q.enqueue(mtu_packet(IpEcn::Ect1, now));
            q.enqueue(mtu_packet(IpEcn::NotEct, now));
        }
        q.c_protection_credit = 1; // the C-queue's turn
        let p = q.dequeue_at(now).unwrap();
        assert_eq!(p.ecn(), Some(IpEcn::CongestionExperienced));
        assert!(q.c_queue.is_empty(), "all Classic packets were dropped");
        // The dropped Classic packets did not consume credit, and the L4S packet
        // was sent with the C-queue empty.
        assert_eq!(q.c_protection_credit, 1);
    }

    #[test_log::test]
    fn test_dualpi2_get_front_size() {
        let now = Instant::now();
        let mut q = queue(frozen_config());
        assert_eq!(q.get_front_size(), None);
        q.enqueue(packet(IpEcn::NotEct, 1000, now));
        assert_eq!(q.get_front_size(), Some(1000));
        q.enqueue(packet(IpEcn::Ect1, 500, now));
        assert_eq!(q.get_front_size(), Some(500), "the L-queue goes first");
        q.c_protection_credit = 1;
        assert_eq!(q.get_front_size(), Some(1000), "the C-queue has credit");
    }

    /// The probability follows the PI2 law, sampled on the `tupdate` grid with the
    /// head-of-line delay as of each grid instant.
    #[test_log::test]
    fn test_dualpi2_probability_update_grid() {
        let config = DualPi2QueueConfig::default();
        let t0 = Instant::now();
        let mut q = queue(config.clone());
        q.enqueue(mtu_packet(IpEcn::NotEct, t0)); // starts the grid at t0
        assert_eq!(q.next_update, Some(t0 + 16 * MS));

        // Just before the 5th grid instant: 4 updates.
        q.enqueue(mtu_packet(IpEcn::NotEct, t0 + 79 * MS));
        assert_eq!(q.next_update, Some(t0 + 80 * MS));
        // At the 5th grid instant: 5 updates.  The head is still the first packet.
        q.enqueue(mtu_packet(IpEcn::NotEct, t0 + 80 * MS));
        assert_eq!(q.next_update, Some(t0 + 96 * MS));

        let (mut p, mut last) = (0.0f64, 0.0f64);
        for i in 1..=5 {
            let qdelay = (16 * i) as f64 / 1000.0;
            p += config.alpha * (qdelay - 0.015) + config.beta * (qdelay - last);
            p = p.clamp(0.0, 1.0);
            last = qdelay;
        }
        assert!((q.prob - p).abs() < 1e-12, "expected {p}, got {}", q.prob);
        assert_eq!(q.last_qdelay, 80 * MS);
    }

    #[test_log::test]
    fn test_dualpi2_probability_uses_larger_delay() {
        let t0 = Instant::now();
        let mut q = queue(Default::default());
        q.enqueue(mtu_packet(IpEcn::Ect1, t0));
        q.enqueue(mtu_packet(IpEcn::NotEct, t0 + 10 * MS));
        q.dequeue_at(t0 + 16 * MS);
        assert_eq!(q.last_qdelay, 16 * MS, "L-queue head delay is larger");
    }

    #[test_log::test]
    fn test_dualpi2_probability_decays_and_idles() {
        let config = DualPi2QueueConfig::default();
        let t0 = Instant::now();
        let mut q = queue(config.clone());
        q.enqueue(mtu_packet(IpEcn::NotEct, t0));
        q.prob = 0.5;
        q.dequeue_at(t0);

        // While idle, p' decays by alpha·target per update, as with a real timer.
        q.dequeue_at(t0 + 32 * MS);
        let expected = 0.5 - 2.0 * config.alpha * 0.015;
        assert!((q.prob - expected).abs() < 1e-12);

        // After a long idle period it reaches 0, and the grid stays aligned.
        let later = t0 + Duration::from_secs(3600) + 5 * MS;
        q.enqueue(mtu_packet(IpEcn::NotEct, later));
        assert_eq!(q.prob, 0.0);
        let next = q.next_update.unwrap();
        assert!(next > later && next - later <= config.tupdate);
        assert_eq!((next - t0).as_nanos() % config.tupdate.as_nanos(), 0);
    }

    #[test_log::test]
    fn test_dualpi2_probability_cap_without_drop_overload() {
        let t0 = Instant::now();
        for (drop_overload, cap) in [(true, 1.0), (false, 0.5)] {
            let mut q = queue(DualPi2QueueConfig {
                drop_overload,
                packet_limit: None,
                ..Default::default()
            });
            // A queue that never drains: the delay grows without bound.
            for i in 0..1000u32 {
                q.enqueue(mtu_packet(IpEcn::NotEct, t0 + i * MS));
            }
            assert_eq!(q.prob, cap);
        }
    }

    #[test_log::test]
    fn test_dualpi2_configure() {
        let now = Instant::now();
        let mut q = queue(Default::default());
        for _ in 0..3 {
            q.enqueue(mtu_packet(IpEcn::Ect1, now));
            q.enqueue(mtu_packet(IpEcn::NotEct, now));
        }

        // Invalid configs are ignored.
        for invalid in [
            DualPi2QueueConfig {
                tupdate: Duration::ZERO,
                ..Default::default()
            },
            DualPi2QueueConfig {
                c_protection: 101,
                ..Default::default()
            },
            DualPi2QueueConfig {
                coupling_factor: f64::NAN,
                ..Default::default()
            },
        ] {
            assert!(DualPi2Queue::<StdPacket>::new(invalid.clone()).is_err());
            q.configure(invalid);
            assert_eq!(q.config, DualPi2QueueConfig::default());
        }

        // Shrinking the limit drops from the C-queue first.
        q.configure(DualPi2QueueConfig {
            packet_limit: Some(4),
            ..Default::default()
        });
        assert_eq!((q.l_queue.len(), q.c_queue.len()), (3, 1));
        q.configure(DualPi2QueueConfig {
            byte_limit: Some(3000),
            ..Default::default()
        });
        assert_eq!((q.l_queue.len(), q.c_queue.len()), (2, 0));
        assert_eq!(q.now_bytes, 3000);

        // Changing the C-queue weight resets the credit.
        q.c_protection_credit = 12345;
        q.configure(DualPi2QueueConfig {
            c_protection: 50,
            ..Default::default()
        });
        assert_eq!(q.c_protection_credit, 0);
    }

    #[cfg(feature = "serde")]
    #[test_log::test]
    fn test_dualpi2_config_serde() {
        let config: DualPi2QueueConfig = serde_json::from_str(
            r#"{"step_threshold": "2ms", "ecn_mask": "AnyEct", "target": "20ms"}"#,
        )
        .unwrap();
        assert_eq!(config.step_threshold, DualPi2StepThreshold::Time(2 * MS));
        assert_eq!(config.ecn_mask, DualPi2EcnMask::AnyEct);
        assert_eq!(config.target, 20 * MS);
        assert_eq!(
            config.packet_limit,
            Some(10000),
            "unset fields keep defaults"
        );

        let config: DualPi2QueueConfig = serde_json::from_str(r#"{"step_threshold": 5}"#).unwrap();
        assert_eq!(config.step_threshold, DualPi2StepThreshold::Packets(5));

        let config = DualPi2QueueConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: DualPi2QueueConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, config);
    }
}
