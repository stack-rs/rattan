//! RED (Random Early Detection) queue with optional Adaptive mode (ARED).
//!
//! # References
//!
//! - RED paper: <https://ieeexplore.ieee.org/stamp/stamp.jsp?tp=&arnumber=251892>
//! - ARED paper: <https://www.icir.org/floyd/papers/adaptiveRed.pdf>
//! - Linux kernel RED/ARED:
//!   [`include/net/red.h`](https://github.com/torvalds/linux/blob/master/include/net/red.h)
//!   and [`net/sched/sch_red.c`](https://github.com/torvalds/linux/blob/master/net/sched/sch_red.c)
//!   (referenced as "kernel" throughout this documentation)
//!
//! # Differences from the Linux kernel implementation
//!
//! ## 1. Fixed-point integers (kernel) vs. floating-point (here)
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | `qavg` | Wlog-scaled integer (`u32`) | `f64` in bytes |
//! | `max_P` | Q0.32 fixed-point (`u32`) | `f64` in `[0.0, 1.0]` |
//! | weight | `Wlog` — weight `W = 1 / (1 << Wlog)` | `w_q` — direct `f64` weight |
//! | division | replaced by `reciprocal_divide()` | native `f64` division |
//!
//! The EWMA update is mathematically equivalent:
//!
//! - Kernel: `qavg += backlog - (qavg >> Wlog)`
//! - Here: `qavg = (1.0 - w_q) * qavg + w_q * backlog`
//!
//! when `w_q = 1.0 / (1 << Wlog)`.
//!
//! ## 2. Idle-period average-queue decay
//!
//! When the queue is empty, both implementations decay the average towards
//! zero by simulating virtual packet departures.  The modelling differs:
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | time unit | cell time via `Scell_log` | `pkt_tx_time` (µs per packet) |
//! | decay computation | precomputed `Stab[256]` lookup table | direct `powf(1.0 - w_q, m)` |
//! | idle-time cap | `Scell_max` (max `255 << Scell_log`) | none (exponent may grow arbitrarily) |
//!
//! Both approaches implement the same formula from the original RED paper
//! (Floyd & Jacobson, 1993, §5 "Calculating the average queue length"):
//!
//! > When a packet arrives and the queue is empty, we compute *m*, the number
//! > of packets that could have been transmitted by the gateway during the
//! > time that the line was free.  We then imagine that *m* packets have
//! > arrived to an empty queue, and calculate the average queue size:
//! >
//! > **avg ← (1 − w_q)^m × avg**
//!
//! This implementation follows the paper literally: `m` is computed as
//! `idle_us / pkt_tx_time` (where `pkt_tx_time` is the transmission time of
//! one average packet), and decay is `avg *= powf(1.0 - w_q, m)`.  The kernel
//! precomputes a logarithmic lookup table (`Stab[]`) indexed by idle duration
//! scaled by `Scell_log` as a fixed-point approximation of `(1 − W)^m`,
//! avoiding both floating-point and the `pow()` call at enqueue time.  The
//! two are mathematically equivalent; the kernel trades some precision for
//! integer-only computation.
//!
//! The `pkt_tx_time` parameter has no direct kernel equivalent.  The kernel
//! separates idle modelling into `Wlog` (EWMA weight) and `Scell_log` (cell
//! granularity), which are independently configurable.  Here, `w_q` controls
//! the decay *rate* and `pkt_tx_time` controls how many virtual departures
//! (`m`) an idle interval represents.
//!
//! ## 3. Random-number generation
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | source | `get_random_u32()` (kernel CSPRNG) | `rand::rngs::StdRng` (ChaCha12) |
//! | seeding | not seedable by userspace | user-configurable `seed` field |
//! | cached? | one `qR` per cycle, reused across packets | fresh `random_range(0.0..1.0)` each check |
//!
//! The kernel draws one random value per "cycle" (from the first packet in the
//! between-threshold region until a mark/drop occurs) and caches it in `qR`.
//! This implementation draws a fresh uniform random value on every
//! `should_drop()` call.  Both produce the same statistical behaviour
//! (geometric inter-drop spacing); the difference is purely an implementation
//! choice.
//!
//! The seedable RNG enables deterministic, reproducible simulation runs.
//!
//! ## 4. Drop-probability computation
//!
//! Both implementations realise the same RED probability curve, but through
//! different arithmetic paths:
//!
//! - **Kernel**: `red_mark_probability()` checks
//!   `((qavg - qth_min) >> Wlog) * qcount >= qR`, where `qR` is uniform in
//!   `[0, qth_delta)`.  This is the "uniform random numbers" (URN) method
//!   with a cached threshold.
//!
//! - **Here**: Classical formula
//!   `p_b = max_p * (avg - min_th) / (max_th - min_th)`,
//!   then `p_a = p_b / (1.0 - count * p_b)`,
//!   and compare `rand_val < p_a`.
//!
//! Both converge to the same geometric inter-drop distribution.  The kernel's
//! approach saves a division per packet via `reciprocal_divide()`; this
//! implementation's approach maps more directly onto the textbook RED
//! description.
//!
//! ## 5. Adaptive RED (ARED) update cadence
//!
//! This is the most behaviourally significant difference.
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | trigger | kernel timer, fires every `HZ/2` (500 ms) | event-driven, on `enqueue()` |
//! | fires when idle? | yes — timer always runs | no — requires a packet arrival |
//! | grid | timer re-arms for `jiffies + HZ/2` | forward-shifted fixed 500 ms grid |
//!
//! **Kernel**: A dedicated timer (`adapt_timer`) fires every 500 ms regardless
//! of whether packets are arriving.  Each firing acquires the qdisc lock,
//! calls `red_adaptative_algo()`, and re-arms.  During prolonged idle periods
//! the kernel therefore decays `max_P` towards 0.01 every 500 ms.
//!
//! **Here**: ARED adjustment happens inside `enqueue()`.  When a packet
//! arrives and at least 500 ms have elapsed since the last adjustment, one
//! call to `update_max_p()` is made and the timer is advanced by exactly 500
//! ms (not reset to current time).  This "fixed grid" approach
//! allows multiple intervals to be caught up over successive packet arrivals
//! after a long idle.  However, if no packets arrive, no adjustments occur.
//!
//! The practical impact: after a long idle followed by a sparse trickle of
//! packets, this implementation may converge `max_P` to its resting value more
//! slowly than the kernel, because each enqueue accounts for at most one 500
//! ms interval.
//!
//! **Why event-driven instead of a dedicated timer?**  This simulator operates
//! on *logical* (simulation) time, not wall-clock time.  A wall-clock timer
//! cannot accurately target a logical-time instant, especially under
//! variable-speed simulation, pause/resume, or replay scenarios.  Worse,
//! spawning an OS timer per queue instance would be prohibitively expensive
//! when simulating hundreds or thousands of flows.
//!
//! ## 6. ARED `max_P` bound enforcement
//!
//! Both clamp `max_P` to `[0.01, 0.50]` (the ARED paper's range), but
//! *when* the bounds are applied differs:
//!
//! - **Kernel**: checks `<= MAX_P_MAX` *before* increasing and `>= MAX_P_MIN`
//!   *before* decreasing.  No explicit post-adjustment clamp — the value can
//!   drift slightly past the nominal bounds in edge cases.
//! - **Here**: always calls `clamp(0.01, 0.5)` unconditionally after every
//!   adjustment, enforcing strict hard bounds.
//!
//! The ARED formulae themselves are identical:
//!
//! - Increase: `max_p += min(0.01, max_p / 4.0)`  (when `qavg > target_max`)
//! - Decrease: `max_p *= 0.9`                     (when `qavg < target_min`)
//! - Targets: `target_min = min_th + 0.4 * (max_th - min_th)`,
//!   `target_max = min_th + 0.6 * (max_th - min_th)`
//!
//! Note: kernel integer arithmetic truncates in `(max_P / 10) * 9` (beta
//! decay), while here `max_p *= 0.9` is exact in `f64`.  The difference is
//! negligible in practice.
//!
//! ## 7. ECN (Explicit Congestion Notification)
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | ECN marking | supported (`TC_RED_ECN` flag) | not supported |
//! | ECN nodrop | supported (`TC_RED_NODROP` flag) | not supported |
//! | mark vs. drop counters | separate `prob_mark`/`prob_drop` | only drops |
//!
//! The kernel can mark ECN-capable packets instead of dropping them.  This
//! implementation always drops.  Adding ECN support would require extending
//! the `Packet` trait with an ECN field.
//!
//! ## 8. Queue architecture
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | structure | classful qdisc → child bfifo | self-contained `VecDeque` |
//! | hard limit | `limit` on child qdisc (bytes) | `packet_limit` + `byte_limit` directly in config |
//!
//! The kernel's RED is a classful qdisc; it owns a child (typically `bfifo`)
//! that holds the actual packet queue.  The `limit` parameter lives on the
//! child.  This implementation is a flat `VecDeque`-based queue with both
//! packet-count and byte-count hard limits checked before the RED drop
//! decision.
//!
//! ## 9. Parameter validation
//!
//! The kernel validates `fls(qth) + Wlog < 32` (overflow prevention given
//! fixed-point arithmetic), `Scell_log < 32`, and all `Stab[]` entries < 32.
//! This implementation validates the semantically equivalent constraints in
//! floating-point terms: `min_th < max_th`, `w_q ∈ (0.0, 1.0]`,
//! `max_p ∈ [0.0, 1.0]`, and `pkt_tx_time > 0`.
//!
//! The kernel's overflow check (`fls(qth) + Wlog < 32`) is specific to
//! `u32` fixed-point arithmetic where `qavg` is stored Wlog-scaled.  With
//! `f64`, overflow is not a concern — the exponent range comfortably covers
//! any practical queue size.  The `Scell_log` and `Stab[]` checks are
//! likewise tied to the kernel's lookup-table idle model and have no
//! equivalent here.  The current validation set is sufficient for a
//! floating-point RED implementation.
//!
//! ## 10. `qcount` reset value
//!
//! - **Kernel**: resets `qcount = 0` after a probabilistic mark.
//! - **Here**: resets `count_packet = -1` after a drop.
//!
//! Both produce **identical behaviour**.  In the kernel, the post-mark
//! sequence is `qcount = 0 → ++qcount = 1 → probability check`, which yields
//! `p_b` for the first packet of the new cycle.  Here, the sequence is
//! `count_packet = -1 → count_packet += 1 = 0 → p_a = p_b / (1.0 - 0·p_b)
//! = p_b`, giving the same first-packet probability.  Subsequent packets
//! accumulate geometrically in both cases.
//!
//! The value `-1` was chosen deliberately over `0` for **internal
//! consistency**: every path that resets `count_packet` (below `min_th`,
//! above `max_th`, hard-limit drop, and probabilistic drop) uses the same
//! sentinel `-1`.  Using a uniform reset value across all branches makes the
//! code easier to reason about and avoids an unnecessary special case.
//!


use std::collections::VecDeque;

use rand::{rngs::StdRng, RngExt, SeedableRng};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, Instant};
use tracing::{debug, warn};

#[cfg(feature = "serde")]
use super::serde_default;
use super::{BwType, PacketQueue};
use crate::cells::Packet;

/// Configuration for a RED (Random Early Detection) queue.
///
/// # Field correspondence with the Linux kernel
///
/// | Field | Kernel equivalent | Notes |
/// |-------|-------------------|-------|
/// | `min_th` | `red_parms.qth_min` | Kernel stores as `u32`; here as `usize`. Default: 7500 (5 × 1500 B). |
/// | `max_th` | `red_parms.qth_max` | Kernel stores as `u32`; here as `usize`. Default: 22500 (15 × 1500 B). |
/// | `max_p` | `red_parms.max_P` | Kernel stores as Q0.32 fixed-point `u32`; here as `f64` in `[0.0, 1.0]`. Default: 0.02. |
/// | `w_q` | `red_parms.Wlog` | Kernel stores log₂ weight as `u8` (weight = 1/(1<<Wlog)); here as direct `f64`. Default: 0.002. |
/// | `pkt_tx_time` | *(no direct equivalent)* | Kernel uses `Scell_log` + `Stab[]` lookup table for idle-period decay. Here: µs per average packet, used as `m = idle_us / pkt_tx_time`. |
/// | `adaptive` | *(no direct equivalent)* | Kernel ARED is always active (via `adapt_timer`); here it is a configurable toggle. |
/// | `packet_limit` | *(no direct equivalent)* | Kernel has a single `q->limit` (from `tc_red_qopt.limit`) applied to the child qdisc. |
/// | `byte_limit` | `q->limit` / `tc_red_qopt.limit` | Kernel's limit is byte-oriented and applied to the child qdisc (bfifo); here applied directly. |
/// | `bw_type` | *(no direct equivalent)* | Configures L2 overhead for bandwidth calculation; simulation-specific. |
/// | `seed` | *(no direct equivalent)* | Deterministic RNG seed; the kernel uses unseedable CSPRNG (`get_random_u32()`). |
///
/// Kernel parameters **not present** in this struct: `Plog` (probability scaling),
/// `Scell_log` (cell-size logarithm), `Stab[]` (precomputed idle-decay table),
/// ECN flags (`TC_RED_ECN`, `TC_RED_NODROP`).  See the module-level documentation
/// for details.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(default))]
#[derive(Debug, Clone)]
pub struct RedQueueConfig {
    pub packet_limit: Option<usize>,
    pub byte_limit: Option<usize>,
    pub w_q: f64,      // queue weight for calculating the average queue length
    pub min_th: usize, // minimum threshold of average queue length
    pub max_th: usize, // maximum threshold of average queue length
    pub max_p: f64,    // maximum probability of dropping a packet
    // Packet transmission time in microseconds.
    // Used to compute the number of "virtual packet departures" during an idle period
    // for average queue length decay: `m = idle_time_us / pkt_tx_time`.
    // The upper layer computes this from link bandwidth `C` and average packet size `avpkt`
    // as `pkt_tx_time = avpkt * 8 / C` (C in Mbps), then passes it down as a fixed config.
    pub pkt_tx_time: f64,
    pub adaptive: bool, // enable adaptive mode (ARED): max_p is adjusted dynamically

    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "serde_default")
    )]
    pub bw_type: BwType,
    #[cfg_attr(feature = "serde", serde(default = "default_red_seed"))]
    pub seed: u64,
}

impl Default for RedQueueConfig {
    fn default() -> Self {
        Self {
            packet_limit: None,
            byte_limit: None,
            w_q: 0.002,
            min_th: 7500,  // 5 * 1500 bytes
            max_th: 22500, // 15 * 1500 bytes
            max_p: 0.02,
            pkt_tx_time: 120.0, // 1500 bytes * 8 / 100Mbps = 120 us
            adaptive: false,
            bw_type: BwType::default(),
            seed: 42,
        }
    }
}

#[cfg(feature = "serde")]
const fn default_red_seed() -> u64 {
    42
}

impl RedQueueConfig {
    fn validate(&self) -> Result<(), &'static str> {
        if self.min_th >= self.max_th {
            return Err("RedQueueConfig: min_th >= max_th");
        }
        if self.w_q <= 0.0 || self.w_q > 1.0 || !self.w_q.is_finite() {
            return Err("RedQueueConfig: w_q must be in (0.0, 1.0] and finite");
        }
        if self.max_p < 0.0 || self.max_p > 1.0 || !self.max_p.is_finite() {
            return Err("RedQueueConfig: max_p must be in [0.0, 1.0] and finite");
        }
        if self.pkt_tx_time <= 0.0 || !self.pkt_tx_time.is_finite() {
            return Err("RedQueueConfig: pkt_tx_time must be > 0 and finite");
        }
        Ok(())
    }

    // A `RedQueueConfig` can be constructed in any of these ways:
    // 1. Struct literal with defaults:
    //      RedQueueConfig { min_th: 100, ..Default::default() }
    // 2. Setter chain (avoids the old giant `new()` with 9+ args):
    //      RedQueueConfig::default().with_min_th(100).with_adaptive(true)
    // 3. Plain default (all fields take their `Default` values):
    //      RedQueueConfig::default()
    pub fn with_packet_limit(mut self, limit: usize) -> Self {
        self.packet_limit = Some(limit);
        self
    }

    pub fn with_byte_limit(mut self, limit: usize) -> Self {
        self.byte_limit = Some(limit);
        self
    }

    pub fn with_w_q(mut self, w_q: f64) -> Self {
        self.w_q = w_q;
        self
    }

    pub fn with_min_th(mut self, min_th: usize) -> Self {
        self.min_th = min_th;
        self
    }

    pub fn with_max_th(mut self, max_th: usize) -> Self {
        self.max_th = max_th;
        self
    }

    pub fn with_max_p(mut self, max_p: f64) -> Self {
        self.max_p = max_p;
        self
    }

    pub fn with_pkt_tx_time(mut self, pkt_tx_time: f64) -> Self {
        self.pkt_tx_time = pkt_tx_time;
        self
    }

    pub fn with_adaptive(mut self, adaptive: bool) -> Self {
        self.adaptive = adaptive;
        self
    }

    pub fn with_bw_type(mut self, bw_type: BwType) -> Self {
        self.bw_type = bw_type;
        self
    }

    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }
}

impl<P: Packet> TryFrom<RedQueueConfig> for RedQueue<P> {
    type Error = &'static str;

    fn try_from(config: RedQueueConfig) -> Result<Self, Self::Error> {
        RedQueue::new(config)
    }
}

#[derive(Debug)]
pub struct RedQueue<P> {
    queue: VecDeque<P>,
    config: RedQueueConfig,
    now_bytes: usize,
    average_queue_length: f64,
    count_packet: i32,                    // number of packets since last dropping
    idle_start: Option<Instant>,          // start time of current idle period
    latest_max_p_update: Option<Instant>, // latest time when max_p was updated (used in adaptive mode), set by first enqueue
    rng: StdRng,
}

impl<P> Default for RedQueue<P>
where
    P: Packet,
{
    fn default() -> Self {
        Self::new(RedQueueConfig::default())
            .expect("RedQueueConfig::default() should never fail validation")
    }
}

impl<P> RedQueue<P>
where
    P: Packet,
{
    fn update_avg(&mut self, packet: &P) {
        if !self.is_empty() {
            self.average_queue_length = (1.0 - self.config.w_q) * self.average_queue_length
                + self.config.w_q * (self.now_bytes as f64);
            return;
        }

        if let Some(idle_start) = self.idle_start {
            let now = packet.get_timestamp();
            let idle_duration = now.saturating_duration_since(idle_start);
            let m = idle_duration.as_micros() as f64 / self.config.pkt_tx_time;
            self.average_queue_length *= f64::powf(1.0 - self.config.w_q, m);
            self.idle_start = Some(now);
        }
    }

    fn should_drop(&mut self) -> bool {
        let avg = self.average_queue_length;
        let min_th = self.config.min_th as f64;
        let max_th = self.config.max_th as f64;
        if avg >= min_th && avg < max_th {
            self.count_packet += 1;
            let p_b = self.config.max_p * (avg - min_th) / (max_th - min_th);
            let p_a = if self.count_packet as f64 * p_b >= 1.0 {
                1.0
            } else {
                p_b / (1.0 - self.count_packet as f64 * p_b)
            };

            let rand_val = self.rng.random_range(0.0..1.0);
            if rand_val < p_a {
                self.count_packet = -1; // first add, then calculate p_a
                true
            } else {
                false
            }
        } else if avg >= max_th {
            self.count_packet = -1;
            true
        } else {
            self.count_packet = -1;
            false
        }
    }

    fn update_max_p(&mut self) {
        let target_min =
            self.config.min_th as f64 + 0.4 * (self.config.max_th - self.config.min_th) as f64;
        let target_max =
            self.config.min_th as f64 + 0.6 * (self.config.max_th - self.config.min_th) as f64;
        if self.average_queue_length > target_max {
            self.config.max_p += (self.config.max_p / 4.0).min(0.01);
        } else if self.average_queue_length < target_min {
            self.config.max_p *= 0.9;
        }
        self.config.max_p = self.config.max_p.clamp(0.01, 0.5);
    }
}

impl<P> PacketQueue<P> for RedQueue<P>
where
    P: Packet,
{
    type Config = RedQueueConfig;

    fn new(config: RedQueueConfig) -> Result<Self, &'static str> {
        config.validate()?;
        debug!(?config, "New RedQueue");
        let seed = config.seed;
        Ok(Self {
            queue: VecDeque::new(),
            config,
            now_bytes: 0,
            average_queue_length: 0.0,
            count_packet: -1,
            idle_start: None,
            latest_max_p_update: None,
            rng: StdRng::seed_from_u64(seed),
        })
    }

    fn configure(&mut self, config: Self::Config) {
        if let Err(e) = config.validate() {
            warn!("RedQueue: discard invalid configure: {}", e);
            return;
        }
        if config.seed != self.config.seed {
            self.rng = StdRng::seed_from_u64(config.seed);
        }
        self.config = config;
    }

    fn is_zero_buffer(&self) -> bool {
        self.config.packet_limit.is_some_and(|limit| limit == 0)
            || self.config.byte_limit.is_some_and(|limit| limit == 0)
    }

    fn enqueue(&mut self, packet: P) {
        self.update_avg(&packet);

        if self.config.adaptive {
            let now = packet.get_timestamp();
            if self.latest_max_p_update.is_none() {
                self.latest_max_p_update = Some(now);
            }
            if now.saturating_duration_since(self.latest_max_p_update.unwrap())
                >= Duration::from_millis(500)
            {
                self.update_max_p();
                // Advance on a fixed 500 ms grid rather than anchoring to the current packet arrival time.
                // Anchoring to now would reset the timer on every trigger and silently skip adjustment intervals
                // when arrivals are sparse (e.g. one packet after a 2 s idle would fire only once instead of catching up over multiple enqueues).
                self.latest_max_p_update =
                    Some(self.latest_max_p_update.unwrap() + Duration::from_millis(500));
            }
        }

        let packet_size = packet.l3_length() + self.get_extra_length();
        let below_hard_limit = self
            .config
            .packet_limit
            .is_none_or(|limit| self.queue.len() < limit)
            && self
                .config
                .byte_limit
                .is_none_or(|limit| self.now_bytes + packet_size <= limit);

        if !below_hard_limit {
            self.count_packet = -1;
            #[cfg(test)]
            tracing::trace!(
                queue_len = self.queue.len(),
                now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) due to hard limit", packet.l3_length(), self.get_extra_length()
            );
            return;
        }

        if self.should_drop() {
            #[cfg(test)]
            tracing::trace!(
                avg = self.average_queue_length,
                count = self.count_packet,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) due to RED algorithm", packet.l3_length(), self.get_extra_length()
            );
            return;
        }
        self.now_bytes += packet_size;
        self.queue.push_back(packet);
        self.idle_start = None;
    }

    fn dequeue_at(&mut self, timestamp: Instant) -> Option<P> {
        if let Some(packet) = self.queue.pop_front() {
            self.now_bytes -= packet.l3_length() + self.get_extra_length();
            if self.is_empty() {
                self.idle_start = Some(timestamp);
            }
            Some(packet)
        } else {
            None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cells::StdPacket;

    fn create_packet(size: usize) -> StdPacket {
        let buf = vec![0u8; size];
        StdPacket::with_timestamp(&buf, Instant::now())
    }

    #[test_log::test]
    fn test_red_queue_basic() {
        let config = RedQueueConfig {
            min_th: 1000,
            max_th: 2000,
            ..Default::default()
        };
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config).unwrap();

        assert!(queue.is_empty());

        let pkt1 = create_packet(500);
        queue.enqueue(pkt1);
        assert!(!queue.is_empty());
        assert_eq!(queue.length(), 1);

        let dequeued = queue.dequeue_at(Instant::now());
        assert!(dequeued.is_some());
        assert!(queue.is_empty());
    }

    #[test_log::test]
    fn test_red_queue_hard_limit_packet() {
        let config = RedQueueConfig {
            packet_limit: Some(2),
            min_th: 100000, // avoid red drop
            max_th: 200000,
            ..Default::default()
        };
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config).unwrap();

        queue.enqueue(create_packet(100));
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 2);

        // This one should be dropped due to packet limit
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 2);
    }

    #[test_log::test]
    fn test_red_queue_hard_limit_byte() {
        let config = RedQueueConfig {
            byte_limit: Some(150),
            min_th: 100000, // avoid red drop
            max_th: 200000,
            ..Default::default()
        };
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config).unwrap();

        queue.enqueue(create_packet(100)); // l3 length 86.
        assert_eq!(queue.length(), 1);

        // This one should be dropped due to byte limit (86 + 86 > 150)
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 1);
    }

    #[test_log::test]
    fn test_red_queue_max_th_drop() {
        let config = RedQueueConfig {
            min_th: 100,
            max_th: 200,
            w_q: 1.0, // max weight, avg matches instantly
            ..Default::default()
        };
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config).unwrap();

        // First packet
        queue.enqueue(create_packet(100));

        // Second packet
        queue.enqueue(create_packet(300));

        // At this point, queue length is 2, now_bytes is high enough.
        // The next enqueue should see average_queue_length > max_th and drop the packet.
        let before_len = queue.length();
        queue.enqueue(create_packet(100));
        assert_eq!(
            queue.length(),
            before_len,
            "Packet should be dropped by RED max_th"
        );
    }

    #[test_log::test]
    fn test_red_queue_min_th_no_drop() {
        let config = RedQueueConfig {
            min_th: 1000,
            max_th: 2000,
            w_q: 1.0, // Instantly reach exact byte size
            ..Default::default()
        };
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config).unwrap();

        // First packet: queue empty, avg remains 0.
        queue.enqueue(create_packet(514)); // L3 size = 514 - 14 (Ethernet header) = 500
        assert_eq!(queue.length(), 1);

        // Second packet: queue has 500 bytes. w_q=1.0 makes avg = 500.
        // 500 < min_th(1000), so it should not drop.
        queue.enqueue(create_packet(414)); // L3 size = 400
        assert_eq!(queue.length(), 2);

        // Check internal state: count_packet is -1 when avg < min_th
        assert_eq!(queue.count_packet, -1);
    }

    #[test_log::test]
    fn test_red_queue_probabilistic_drop() {
        let config = RedQueueConfig {
            min_th: 100,
            max_th: 300,
            max_p: 0.5,
            w_q: 1.0,
            ..Default::default()
        };
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config).unwrap();

        // First packet: queue empty, avg = 0. L3 size = 200.
        queue.enqueue(create_packet(214));
        assert_eq!(queue.length(), 1);

        let mut drop_count = 0;
        let total_packets = 1000;

        for _ in 0..total_packets {
            // enqueue packets with L3 size 0 (total size 14).
            // now_bytes stays at 200. w_q=1.0 makes avg exactly 200.
            // 100 (min_th) <= avg(200) < 300 (max_th), entering probabilistic drop zone.
            let before = queue.length();
            queue.enqueue(create_packet(14));
            if queue.length() == before {
                drop_count += 1;
            }
        }

        // Calculate expected drop count based on RED algorithm
        // When avg = 200, min_th = 100, max_th = 300, max_p = 0.5
        // p_b = max_p * (avg - min_th) / (max_th - min_th) = 0.5 * 100 / 200 = 0.25
        // Due to the uniform distribution of inter-drop times in RED,
        // the expected drop rate is 2 * p_b / (1 + p_b)
        // For p_b = 0.25, expected drop rate = 0.5 / 1.25 = 0.4 (40%)
        let p_b = 0.25;
        let expected_drop_rate = (2.0 * p_b) / (1.0 + p_b);
        let expected_drop_count = (total_packets as f64 * expected_drop_rate) as usize;

        // Allow reasonable statistical variation (±3 standard deviations, 99.7% confidence)
        // Standard deviation for binomial distribution: sqrt(n * p * (1-p))
        let std_dev =
            (total_packets as f64 * expected_drop_rate * (1.0 - expected_drop_rate)).sqrt();
        let margin = (3.0 * std_dev) as usize;

        let lower_bound = expected_drop_count.saturating_sub(margin);
        let upper_bound = expected_drop_count + margin;

        // Verify drop count is within expected statistical range
        assert!(
            drop_count >= lower_bound && drop_count <= upper_bound,
            "Drop count {} should be in range [{}, {}] (expected {} ± {} based on RED algorithm with p_b=0.25)",
            drop_count, lower_bound, upper_bound, expected_drop_count, margin
        );

        // Keep original assertions as sanity checks
        assert!(
            drop_count > 0,
            "Should have dropped some packets probabilistically"
        );
        assert!(drop_count < total_packets, "Should not drop all packets");
    }

    #[test_log::test]
    fn test_red_queue_adaptive_max_p_increase() {
        let config = RedQueueConfig {
            min_th: 100,
            max_th: 200,
            max_p: 0.02,
            w_q: 1.0, // Instantly update avg
            adaptive: true,
            ..Default::default()
        };
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config).unwrap();

        // enqueue to make average_queue_length > target_max
        // target_max = min_th + 0.6 * (max_th - min_th) = 100 + 60 = 160
        // We make avg = 180 (L3 size 180, total 194)
        queue.enqueue(create_packet(194));
        assert_eq!(queue.average_queue_length, 0.0); // First enqueue updates avg based on empty queue rule (avg=0).

        // Second enqueue updates avg to 180
        queue.enqueue(create_packet(14));
        assert_eq!(queue.average_queue_length, 180.0);

        // Set latest_max_p_update artificially back, then enqueue a packet with current timestamp
        let mut pkt3 = create_packet(14);
        pkt3.delay_until(queue.latest_max_p_update.unwrap() + Duration::from_millis(600));

        let before_max_p = queue.config.max_p;

        // Third enqueue triggers update_max_p
        queue.enqueue(pkt3);

        let after_max_p = queue.config.max_p;
        assert!(
            after_max_p > before_max_p,
            "max_p should increase when avg > target_max"
        );
    }

    #[test_log::test]
    fn test_red_queue_adaptive_max_p_decrease() {
        let config = RedQueueConfig {
            min_th: 100,
            max_th: 200,
            max_p: 0.05, // Starting with a high max_p
            w_q: 1.0,
            adaptive: true,
            ..Default::default()
        };
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config).unwrap();

        // enqueue to make average_queue_length < target_min
        // target_min = min_th + 0.4 * (max_th - min_th) = 100 + 40 = 140
        // We make avg = 120 (L3 size 120, total 134)
        queue.enqueue(create_packet(134));
        assert_eq!(queue.average_queue_length, 0.0);

        queue.enqueue(create_packet(14));
        assert_eq!(queue.average_queue_length, 120.0);

        // Enqueue a packet with timestamp 600ms later to trigger update_max_p
        let mut pkt3 = create_packet(14);
        pkt3.delay_until(queue.latest_max_p_update.unwrap() + Duration::from_millis(600));

        let before_max_p = queue.config.max_p;

        queue.enqueue(pkt3);

        let after_max_p = queue.config.max_p;
        assert!(
            after_max_p < before_max_p,
            "max_p should decrease when avg < target_min"
        );
    }
}
