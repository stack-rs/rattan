//! PIE (Proportional Integral controller Enhanced) queue.
//!
//! # References
//!
//! - PIE paper:
//!   <https://ieeexplore.ieee.org/document/6602305>
//! - RFC 8033 (PIE AQM):
//!   <https://www.rfc-editor.org/info/rfc8033>
//! - Linux kernel PIE:
//!   [`include/net/pie.h`](https://github.com/torvalds/linux/blob/master/include/net/pie.h)
//!   and [`net/sched/sch_pie.c`](https://github.com/torvalds/linux/blob/master/net/sched/sch_pie.c)
//!   (referenced as "kernel" throughout this documentation)
//!
//! # RFC 8033 version: Appendix A vs. Appendix B
//!
//! RFC 8033 contains two descriptions of the PIE algorithm:
//!
//! - **Appendix A** is the "original paper" algorithm: configurable α/β with
//!   *dynamic* per-probability-scaling, accumulated-probability-based drop
//!   decisions, and an optional timestamp-based delay estimation path.
//!
//! - **Appendix B** is a simplified pseudo-code reference implementation:
//!   fixed `α = 0.125`, `β = 1.25`, *static* `p_increment` damping tables,
//!   per-packet random drop (no accumulation), and always drain-rate-based
//!   delay estimation.
//!
//! The kernel follows **Appendix A** (with its own extensions).  This
//! implementation follows **Appendix B**.  This is the most fundamental
//! design difference and explains nearly every other divergence listed below.
//!
//! # Differences from the Linux kernel implementation
//!
//! The 16 differences below fall into three categories:
//!
//! | Category | Sections | Explanation |
//! |----------|----------|-------------|
//! | **RFC 8033 Appendix A vs. B** | §3, §4, §5, §12 | The kernel follows Appendix A; this implementation follows Appendix B. These are deliberate design choices, not omissions. |
//! | **Kernel-specific extensions** | §6, §7, §8, §11, §13 | Features the Linux kernel added beyond what either RFC appendix describes. |
//! | **Implementation / architectural choices** | §1, §2, §9, §10, §14, §15, §16 | Differences arising from the simulation context (floating-point, event-driven, seedable RNG) or architectural constraints. |
//!
//! ## 1. Fixed-point integers (kernel) vs. floating-point (here)
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | `prob` | `u64` scaled by `MAX_PROB` (`U64_MAX >> 8`) | `f64` in `[0.0, 1.0]` |
//! | `avg_dq_rate` | `u32` scaled by `PIE_SCALE` (shift 8) | `f64` in B/s |
//! | delay | `psched_time_t` (kernel time ticks) | `f64` in seconds |
//! | `burst_time` | `psched_time_t` ticks | `f64` in milliseconds |
//! | EWMA for drain rate | `avg = (avg - (avg >> 3)) + (count >> 3)` | `avg = 0.875 * avg + 0.125 * rate` |
//! | `p_increment` damping | dynamic α/β bit-shifts (see §4) | `f64` division by `2048`/`512`/`…` |
//!
//! The EWMA update is mathematically identical (`1/8 = 0.125` weight).  The
//! damping thresholds are also identical (see §4 below).
//!
//! ## 2. Drop-probability update trigger
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | trigger | dedicated kernel timer, fires every `tupdate` jiffies | event-driven, on `enqueue()` |
//! | fires when idle? | yes — timer always runs | no — requires a packet arrival |
//! | grid | timer re-arms for `jiffies + tupdate` | forward-shifted fixed `t_update` grid |
//! | first fire | 500 ms after init | on first enqueue |
//!
//! **Kernel**: A dedicated timer (`adapt_timer`) fires every `tupdate`
//! regardless of whether packets are arriving.  During prolonged idle periods
//! the kernel therefore continues to decay `prob` and update state.
//!
//! **Here**: Probability update happens inside `enqueue()`.  When a packet
//! arrives and at least `t_update` has elapsed since the last update, one
//! call to `update_drop_probability()` is made and the timer is advanced by
//! exactly `t_update` (not reset to current time).  This "fixed grid"
//! approach allows multiple intervals to be caught up over successive packet
//! arrivals after a long idle.  However, if no packets arrive, no updates
//! occur.
//!
//! **Why event-driven instead of a dedicated timer?**  This simulator operates
//! on *logical* (simulation) time, not wall-clock time.  A wall-clock timer
//! cannot accurately target a logical-time instant, especially under
//! variable-speed simulation, pause/resume, or replay scenarios.  Spawning an
//! OS timer per queue instance would be prohibitively expensive when
//! simulating hundreds or thousands of flows.
//!
//! ## 3. Alpha and Beta: configurable (kernel) vs. fixed (here)
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | definition | `struct pie_params.alpha`, `.beta` (user-tunable 0–32) | `tilde_alpha = 0.125`, `tilde_beta = 1.25` (hard-coded) |
//! | internal scaling | `(param * MAX_PROB / PSCHED_TICKS_PER_SEC) >> 4` | direct `f64` |
//! | defaults | α = 2, β = 20 → 2/16 = 0.125, 20/16 = 1.25 | 0.125, 1.25 |
//! | tuning via | `tc qdisc ... pie alpha N beta M` | not configurable |
//!
//! The **default** values are equivalent: `2/16 = 0.125`, `20/16 = 1.25`.
//! However, the kernel exposes α and β as user-tunable knobs and dynamically
//! scales them based on current probability (described in §4).  This
//! implementation hard-codes the RFC 8033 Appendix B base values and applies
//! static damping to `p_increment` instead.
//!
//! ## 4. Probability-increment damping
//!
//! Both implementations damp the probability adjustment when `prob` is small,
//! but through different mechanisms (Appendix A dynamically scales α/β;
//! Appendix B statically divides `p_increment` — thresholds and effective
//! damping factors are mathematically identical):
//!
//! **Kernel (Appendix A — dynamic α/β scaling):**
//!
//! When `prob < MAX_PROB / 10`:
//! 1. α and β are halved (`>>= 1`).
//! 2. Then repeatedly quartered (`>>= 2`) for each power-of-10 threshold:
//!    `prob < MAX_PROB / 100` → quarter again, … up to `MAX_PROB / 10⁶`.
//!
//! **Here (Appendix B — static `p_increment` division):**
//!
//! `p_increment` is divided by a precomputed factor depending on `p`:
//!
//! | `p` range | Division factor |
//! |-----------|----------------:|
//! | `p ≥ 0.1` | 1 (no damping) |
//! | `0.01 ≤ p < 0.1` | 2 |
//! | `0.001 ≤ p < 0.01` | 8 |
//! | `0.0001 ≤ p < 0.001` | 32 |
//! | `0.00001 ≤ p < 0.0001` | 128 |
//! | `0.000001 ≤ p < 0.00001` | 512 |
//! | `p < 0.000001` | 2048 |
//!
//! The thresholds *and* the effective damping factors are **mathematically
//! identical** between the two approaches.  The kernel modifies α/β before
//! computing the delta; this implementation computes the full delta first,
//! then divides.  Because `delta = α·Δcur + β·Δold`, scaling α and β by
//! factor *k* is equivalent to scaling the delta by *k*.
//!
//! ## 5. Drop decision: probability accumulation (kernel) vs. per-packet random (here)
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | mechanism | `accu_prob` accumulates probability across packets | `rand < p` each packet |
//! | drop trigger | accumulated value crosses threshold | immediate comparison |
//! | burst tolerance | natural — multiple low-prob packets needed to trigger | via explicit `burst_allowance` check |
//! | RFC version | Appendix A | Appendix B |
//!
//! **Kernel**: Maintains an `accu_prob` counter in `pie_drop_early()`.  Each
//! arriving packet adds `local_prob` (possibly scaled by `bytemode`) to the
//! accumulator.  A packet is dropped only when `accu_prob` crosses a
//! threshold (`(MAX_PROB / 2) * 17`).  This naturally spaces out drops —
//! several low-probability packets must arrive before one is dropped.
//!
//! **Here**: Each `should_drop()` call draws a fresh uniform random value and
//! compares directly against `p`.  This is the simpler Appendix B approach
//! and produces the same statistical drop rate over many packets; the
//! difference is that drops are less evenly spaced (potentially clumpier)
//! without accumulation.
//!
//! ## 6. ECN (Explicit Congestion Notification)
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | ECN marking | supported (`TCA_PIE_ECN` flag) | not supported |
//! | mark condition | `prob ≤ MAX_PROB / 10` + packet is ECN-capable | N/A |
//! | mark counter | `ecn_mark` stat | N/A |
//!
//! The kernel can mark ECN-capable packets (setting the CE codepoint) instead
//! of dropping them when the drop probability is moderate.  This
//! implementation always drops.  Adding ECN support would require extending
//! the `Packet` trait with an ECN field.
//!
//! ## 7. Bytemode (packet-size-scaled probability)
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | feature | optional `bytemode` flag (`TCA_PIE_BYTEMODE`) | not supported |
//! | scaling | `local_prob = prob * pkt_size / mtu` (for packets ≤ MTU) | N/A |
//!
//! When enabled, the kernel scales the drop probability proportionally to the
//! packet size, making larger packets more likely to be dropped.  This
//! implementation always treats all packets equally regardless of size.
//!
//! ## 8. Non-linear boost for high delay (>250 ms)
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | trigger | `qdelay > 250 ms` | not implemented |
//! | effect | `delta += 2%` of max probability | N/A |
//!
//! When the estimated queue delay exceeds 250 ms, the kernel adds an extra
//! 2% to the probability delta to more aggressively counteract severe
//! congestion.  This boost is applied *after* the α/β damping, so it is not
//! subject to the same scaling.  This implementation (following Appendix B)
//! has no such non-linear term; the sole source of `p_increment` is the
//! proportional-integral calculation, damped as described in §4.
//!
//! This is one of the more behaviourally significant differences: under
//! severe congestion (bufferbloat), the kernel will increase its drop
//! probability faster than this implementation.
//!
//! ## 9. Rapid decay condition and factor
//!
//! Both implementations reduce the drop probability when the queue is
//! consistently uncongested, but the *trigger condition* and *decay factor*
//! differ:
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | condition | `qdelay == 0` **and** `qdelay_old == 0` (exactly zero) | `cur_del < ref_del / 2` **and** `old_del < ref_del / 2` |
//! | guard | `update_prob == true` (no overflow/underflow this round) | none |
//! | retention factor | `63/64` ≈ 0.9844 (`prob -= prob / 64`) | `0.98` (`p *= 0.98`) |
//!
//! The kernel only decays when delay is *exactly* zero for two consecutive
//! update periods — a stricter condition.  This implementation decays
//! whenever the delay is below half the target (7.5 ms at defaults), which
//! is a looser condition.  Combined with the faster decay factor (0.98 vs.
//! 0.9844), this implementation may reduce `p` more aggressively during
//! lightly-loaded periods.
//!
//! ## 10. Burst allowance reduction mechanism
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | reduced in | `pie_process_dequeue()` — every dequeue | `update_drop_probability()` — only when update fires |
//! | reduced by | `dtime` (inter-dequeue interval, in psched ticks) | `elapsed_ms` (`t_update`, in milliseconds) |
//! | floor | 0 (via `max_t(psched_time_t, ...)`) | 0 (via `.max(0.0)`) |
//! | recharge condition | `prob == 0 && delay < target/2 && delay_old < target/2` | `p < EPSILON && cur_del < ref_del/2 && old_del < ref_del/2` |
//! | recharge value | 150 ms (`PSCHED_TICKS_PER_SEC * 150 / 1000`) | `max_burst` (default 150.0 ms) |
//!
//! The kernel reduces `burst_time` on *every* dequeue by the actual
//! inter-departure time, so burst allowance reflects real-time packet
//! departures.  This implementation reduces `burst_allowance` only during
//! probability updates, by the `t_update` interval.  During periods where
//! updates fire regularly (steady packet arrivals), both behave similarly.
//! During sparse arrivals where updates are caught up in bursts, the
//! reduction granularity may differ.
//!
//! ## 11. Variable reset on sustained good behaviour
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | full variable reset | `pie_vars_init()` when `qdelay < target/2`, `qdelay_old < target/2`, `prob == 0`, and rate estimator has a valid reading | not performed |
//! | effect of reset | clears `prob`, `avg_dq_rate`, `dq_count`, `dq_tstamp`, `accu_prob`; resets `burst_time` to 150 ms | only `burst_allowance` is recharged to `max_burst` |
//!
//! The kernel fully resets all PIE state variables when the queue has been
//! well-behaved for long enough.  This implementation only recharges the
//! burst allowance (see §10) and leaves `avg_drate`, `p`, and measurement
//! state intact.  After a prolonged idle or light-load period followed by
//! sudden congestion, this implementation may react with a stale (possibly
//! too-low) `avg_drate`, causing a transient over-estimation of delay.
//!
//! ## 12. Delay estimation: always drain-rate-based vs. dual-mode
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | timestamp mode | supported (`dq_rate_estimator = false`) | not supported |
//! | drain-rate mode | supported (`dq_rate_estimator = true`) | always active |
//! | timestamp source | `pie_skb_cb.enqueue_time` from skb control block | N/A |
//! | drain-rate threshold | 16 KiB (`QUEUE_THRESHOLD`) | 16 KiB (`dq_threshold`) |
//! | measurement entry | `backlog >= 16384` | `now_bytes > 16384` |
//! | EWMA weight | `1/8` (0.125) | `0.125` |
//!
//! The kernel can optionally measure delay directly from per-packet enqueue
//! timestamps when the drain-rate estimator is disabled.  This
//! implementation always estimates delay as `now_bytes / avg_drate`,
//! equivalent to the kernel's `dq_rate_estimator = true` mode.
//!
//! The measurement cycle entry condition differs by one byte: the kernel
//! uses `>=` while this implementation uses `>`.  With standard MTU packets
//! (~1514 B with L2 overhead) this off-by-one is not practically observable.
//!
//! ## 13. Update guard: zero delay with non-zero backlog
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | guard | when `qdelay == 0` but `backlog != 0`, skip this round's probability update | not implemented |
//!
//! The kernel refrains from updating `prob` when the drain-rate estimator
//! yields zero delay but the queue is not actually empty — this indicates
//! the estimator has not yet converged.  This implementation always applies
//! the probability update; if `avg_drate ≈ 0` (estimator not yet
//! initialized), `cur_del` is forced to 0 via the `abs(avg_drate) < EPSILON`
//! check.
//!
//! ## 14. Random-number generation
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | source | `get_random_u64()` (kernel CSPRNG) | `rand::rngs::StdRng` (ChaCha12) |
//! | seeding | not seedable by userspace | user-configurable `seed` field |
//!
//! The kernel uses the kernel's CSPRNG for drop decisions.  This
//! implementation uses a seedable ChaCha12 RNG, enabling deterministic,
//! reproducible simulation runs.
//!
//! ## 15. Queue architecture
//!
//! | Aspect | Kernel | This implementation |
//! |--------|--------|---------------------|
//! | structure | full `Qdisc` with netlink configuration | self-contained `VecDeque` |
//! | hard limit | single `limit` (packets) | dual `packet_limit` + `byte_limit` |
//! | statistics | `tc_pie_xstats` (prob, delay, packets_in, dropped, overlimit, maxq, ecn_mark) | none |
//!
//! The kernel's PIE is a full Linux qdisc with netlink-based configuration
//! (`tc qdisc ... pie`), statistics export, and lifecycle management.
//! This implementation is a queue *component* within a larger simulation
//! framework; it stores packets in an internal `VecDeque` and exposes the
//! `PacketQueue` trait.
//!
//! The kernel's single `limit` is a packet count.  This implementation adds
//! an independent `byte_limit` for byte-oriented capacity management.
//!
//! ## 16. Configurable parameters
//!
//! The kernel exposes several parameters that have no counterpart here:
//!
//! | Kernel parameter | `TCA_PIE_*` attribute | Purpose | Status here |
//! |------------------|-----------------------|---------|-------------|
//! | `alpha` | `TCA_PIE_ALPHA` | PI controller α gain (0–32) | fixed at 0.125 |
//! | `beta` | `TCA_PIE_BETA` | PI controller β gain (0–32) | fixed at 1.25 |
//! | `ecn` | `TCA_PIE_ECN` | enable ECN marking | not supported |
//! | `bytemode` | `TCA_PIE_BYTEMODE` | scale drop prob by packet size | not supported |
//! | `dq_rate_estimator` | `TCA_PIE_DQ_RATE_ESTIMATOR` | enable drain-rate-based delay estimation | always on |
//!
//! Parameters present here but not in the kernel:
//!
//! | Parameter | Purpose |
//! |-----------|---------|
//! | `bw_type` | configures L2 overhead for bandwidth calculation |
//! | `seed` | deterministic RNG seed for reproducible simulation |
//! | `byte_limit` | byte-oriented hard queue limit (kernel uses single packet `limit`) |

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

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(default))]
#[derive(Debug, Clone)]
pub struct PieQueueConfig {
    pub packet_limit: Option<usize>,
    pub byte_limit: Option<usize>,
    pub ref_del: f64,   // target delay (sec)
    pub max_burst: f64, // MAX_BURST (ms)
    #[cfg_attr(feature = "serde", serde(with = "crate::utils::serde::duration"))]
    pub t_update: Duration, // update interval
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "serde_default")
    )]
    pub bw_type: BwType,
    #[cfg_attr(feature = "serde", serde(default = "default_pie_seed"))]
    pub seed: u64,
}

impl Default for PieQueueConfig {
    fn default() -> Self {
        Self {
            packet_limit: None,
            byte_limit: None,
            ref_del: 0.015, // RFC 8033
            max_burst: 150.0,
            t_update: Duration::from_millis(15),
            bw_type: BwType::default(),
            seed: 42,
        }
    }
}

#[cfg(feature = "serde")]
const fn default_pie_seed() -> u64 {
    42
}

impl PieQueueConfig {
    fn validate(&self) -> Result<(), &'static str> {
        if self.ref_del <= 0.0 || !self.ref_del.is_finite() {
            return Err("PieQueueConfig: ref_del must be > 0 and finite");
        }
        if self.max_burst < 0.0 || !self.max_burst.is_finite() {
            return Err("PieQueueConfig: max_burst must be >= 0 and finite");
        }
        if self.t_update <= Duration::ZERO {
            return Err("PieQueueConfig: t_update must be > 0");
        }
        Ok(())
    }

    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
        ref_del: f64,
        max_burst: f64,
        t_update: Duration,
        bw_type: BwType,
        seed: u64,
    ) -> Self {
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
            ref_del,
            max_burst,
            t_update,
            bw_type,
            seed,
        }
    }
}

impl<P: Packet> TryFrom<PieQueueConfig> for PieQueue<P> {
    type Error = &'static str;

    fn try_from(config: PieQueueConfig) -> Result<Self, Self::Error> {
        PieQueue::new(config)
    }
}

#[derive(Debug)]
pub struct PieQueue<P> {
    queue: VecDeque<P>,
    config: PieQueueConfig,
    now_bytes: usize,
    old_del: f64,                       // previous delay (sec)
    p: f64,                             // current drop probability
    dq_count: usize,                    // departure count (bytes)
    start_update: Option<Instant>,      // start time of t_update, set by first enqueue
    start_measurement: Option<Instant>, // Some(Instant) when in a measurement cycle, None when quit
    avg_drate: f64,
    burst_allowance: f64,
    rng: StdRng,
}

impl<P> Default for PieQueue<P>
where
    P: Packet,
{
    fn default() -> Self {
        Self::new(PieQueueConfig::default())
            .expect("PieQueueConfig::default() should never fail validation")
    }
}

impl<P> PieQueue<P>
where
    P: Packet,
{
    fn update_drop_probability(&mut self) {
        let elapsed_ms = self.config.t_update.as_secs_f64() * 1000.0;

        let cur_del = if self.avg_drate.abs() < f64::EPSILON {
            0.0
        } else {
            self.now_bytes as f64 / self.avg_drate
        };

        let tilde_alpha = 0.125; // base value of alpha (Hz, 1/sec)
        let tilde_beta = 1.25; // base value of beta (Hz, 1/sec)
        let mut p_increment =
            tilde_alpha * (cur_del - self.config.ref_del) + tilde_beta * (cur_del - self.old_del);
        if self.p < 0.000001 {
            p_increment /= 2048.0;
        } else if self.p < 0.00001 {
            p_increment /= 512.0;
        } else if self.p < 0.0001 {
            p_increment /= 128.0;
        } else if self.p < 0.001 {
            p_increment /= 32.0;
        } else if self.p < 0.01 {
            p_increment /= 8.0;
        } else if self.p < 0.1 {
            p_increment /= 2.0;
        }

        // RFC 8033 Section 5.5: Cap Drop Adjustment
        if self.p >= 0.1 {
            p_increment = p_increment.min(0.02);
        }

        self.p += p_increment;

        // RFC 8033 Section 4.2: Exponential decay when system is not congested
        if cur_del < self.config.ref_del / 2.0 && self.old_del < self.config.ref_del / 2.0 {
            self.p *= 0.98;
        }
        self.p = self.p.clamp(0.0, 1.0);

        // RFC 8033 Section 4.4: Burst Tolerance
        if self.p < f64::EPSILON
            && cur_del < self.config.ref_del / 2.0
            && self.old_del < self.config.ref_del / 2.0
        {
            self.burst_allowance = self.config.max_burst;
        } else {
            self.burst_allowance = (self.burst_allowance - elapsed_ms).max(0.0);
        }
        self.old_del = cur_del;
        self.start_update = Some(self.start_update.unwrap() + self.config.t_update);
    }

    fn should_drop(&mut self) -> bool {
        // RFC 8033 Section 4.4: Enqueue packet bypassing random drop if burst_allow > 0
        if self.burst_allowance > f64::EPSILON {
            return false;
        }

        // RFC 8033 Section 4.1: Bypass random drop logic to be work conserving
        // MEAN_PKTSIZE is generally considered to be 1500 bytes (standard MTU) in RFCs.
        // We add extra_length to align with how now_bytes is calculated (L2 vs L3).
        let mean_pktsize = 1500 + self.get_extra_length();
        let bypass_drop = (self.old_del < self.config.ref_del / 2.0 && self.p < 0.2)
            || self.now_bytes <= 2 * mean_pktsize;
        if bypass_drop {
            return false;
        }

        let rand_val = self.rng.random_range(0.0..1.0);
        rand_val < self.p
    }

    fn update_avg_drate(&mut self, pkt_size: usize, now: Instant) {
        let dq_threshold = 16384; // 16 KiB

        // Enter a measurement cycle
        if self.now_bytes > dq_threshold && self.start_measurement.is_none() {
            self.start_measurement = Some(now);
            self.dq_count = 0;
        }

        // Update departure rate if we are in a measurement cycle
        if let Some(start) = self.start_measurement {
            self.dq_count += pkt_size;
            if self.dq_count >= dq_threshold {
                let dq_int = now.saturating_duration_since(start).as_secs_f64();
                if dq_int > f64::EPSILON {
                    let dq_rate = self.dq_count as f64 / dq_int;
                    if self.avg_drate.abs() < f64::EPSILON {
                        self.avg_drate = dq_rate;
                    } else {
                        let epsilon = 0.125;
                        self.avg_drate = (1.0 - epsilon) * self.avg_drate + epsilon * dq_rate;
                    }
                    self.start_measurement = None;
                    self.dq_count = 0;
                }
            }

            // Exit measurement cycle if queue length drops below threshold
            if self.now_bytes < dq_threshold {
                self.start_measurement = None;
                self.dq_count = 0;
            }
        }
    }
}

impl<P> PacketQueue<P> for PieQueue<P>
where
    P: Packet,
{
    type Config = PieQueueConfig;

    fn new(config: PieQueueConfig) -> Result<Self, &'static str> {
        config.validate()?;
        debug!(?config, "New PieQueue");
        let max_burst = config.max_burst;
        let seed = config.seed;
        Ok(Self {
            queue: VecDeque::new(),
            config,
            now_bytes: 0,
            old_del: 0.0,
            p: 0.0,
            dq_count: 0,
            start_update: None,
            start_measurement: None,
            avg_drate: 0.0,
            burst_allowance: max_burst,
            rng: StdRng::seed_from_u64(seed),
        })
    }

    fn configure(&mut self, config: Self::Config) {
        if let Err(e) = config.validate() {
            warn!("PieQueue: discard invalid configure: {}", e);
            return;
        }
        if config.seed != self.config.seed {
            self.rng = StdRng::seed_from_u64(config.seed);
        }
        self.config = config;
        self.burst_allowance = self.burst_allowance.min(self.config.max_burst);
    }

    fn is_zero_buffer(&self) -> bool {
        self.config.packet_limit.is_some_and(|limit| limit == 0)
            || self.config.byte_limit.is_some_and(|limit| limit == 0)
    }

    fn enqueue(&mut self, packet: P) {
        // Simulate time-driven with event-driven approach by using circular update logic
        if self.start_update.is_none() {
            self.start_update = Some(packet.get_timestamp());
        }
        let mut interval_update = packet
            .get_timestamp()
            .saturating_duration_since(self.start_update.unwrap());
        while interval_update >= self.config.t_update {
            self.update_drop_probability();
            interval_update = packet
                .get_timestamp()
                .saturating_duration_since(self.start_update.unwrap());
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
                p = self.p,
                old_delay = self.old_del,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) due to PIE algorithm", packet.l3_length(), self.get_extra_length()
            );
            return;
        }
        self.now_bytes += packet_size;
        self.queue.push_back(packet);
    }

    fn dequeue_at(&mut self, timestamp: Instant) -> Option<P> {
        if let Some(packet) = self.queue.pop_front() {
            let pkt_size = packet.l3_length() + self.get_extra_length();
            self.now_bytes -= pkt_size;
            self.update_avg_drate(pkt_size, timestamp);
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
    fn test_pie_queue_basic() {
        let config = PieQueueConfig::default();
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config).unwrap();
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
    fn test_pie_queue_hard_limit_packet() {
        let config = PieQueueConfig {
            packet_limit: Some(2),
            ..Default::default()
        };
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config).unwrap();

        queue.enqueue(create_packet(100));
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 2);

        // This one should be dropped due to packet limit
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 2);
    }

    #[test_log::test]
    fn test_pie_queue_hard_limit_byte() {
        let config = PieQueueConfig {
            byte_limit: Some(150),
            ..Default::default()
        };
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config).unwrap();

        queue.enqueue(create_packet(100)); // l3 length 86.
        assert_eq!(queue.length(), 1);

        // This one should be dropped due to byte limit (86 + 86 > 150)
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 1);
    }

    #[test_log::test]
    fn test_pie_queue_burst_allowance() {
        let config = PieQueueConfig::default();
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config).unwrap();

        // Force a high drop probability
        queue.p = 1.0;
        queue.burst_allowance = 100.0;

        // burst_allowance > 0 bypasses random drop
        queue.enqueue(create_packet(1500));
        assert_eq!(queue.length(), 1);

        // Fill queue > 2 * 1500 bytes to bypass work conserving logic later
        queue.enqueue(create_packet(1500));
        queue.enqueue(create_packet(1500));
        assert_eq!(queue.length(), 3);

        queue.burst_allowance = 0.0;
        // With burst_allowance = 0.0, queue > 2 * 1500 bytes, and p = 1.0, it should drop
        queue.enqueue(create_packet(1500));
        assert_eq!(queue.length(), 3);
    }

    #[test_log::test]
    fn test_pie_queue_work_conserving() {
        let config = PieQueueConfig::default();
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config.clone()).unwrap();
        queue.p = 1.0;
        queue.burst_allowance = 0.0;

        // bypass_drop handles queue.now_bytes <= 3000
        queue.enqueue(create_packet(1500));
        assert_eq!(queue.length(), 1);
        queue.enqueue(create_packet(1500));
        assert_eq!(queue.length(), 2);

        // For the 3rd element, now_bytes is 3000, so it bypasses based on byte length (<= 3000)
        queue.enqueue(create_packet(1500));
        assert_eq!(queue.length(), 3);

        // For the 4th element, now_bytes is 4500, so it does not bypass based on byte length
        // Since p = 1.0, it drops
        queue.enqueue(create_packet(1500));
        assert_eq!(queue.length(), 3);

        // Now test bypass_drop condition: old_del < ref_del/2 and p < 0.2
        queue.p = 0.15;
        queue.old_del = config.ref_del / 3.0;
        queue.enqueue(create_packet(1500));
        assert_eq!(queue.length(), 4);
    }

    #[test_log::test]
    fn test_pie_queue_avg_drate_update() {
        let config = PieQueueConfig::default();
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config).unwrap();

        queue.enqueue(create_packet(10014)); // l3 length 10000
        queue.enqueue(create_packet(10014)); // l3 length 10000
        queue.enqueue(create_packet(10014)); // l3 length 10000
        assert_eq!(queue.now_bytes, 30000);

        // First dequeue triggers start of measurement cycle
        assert!(queue.start_measurement.is_none());
        let deq_time1 = Instant::now();
        queue.dequeue_at(deq_time1); // dequeues 10000 bytes
        assert!(queue.start_measurement.is_some());
        assert_eq!(queue.now_bytes, 20000);

        // Simulate time advancing for the next measurement
        let mut pkt2 = create_packet(10014);
        pkt2.delay_until(deq_time1 + Duration::from_millis(10));

        // Enqueue the packet with advanced timestamp to update queue state
        queue.enqueue(pkt2);

        // Second dequeue triggers calculation of avg_drate
        queue.dequeue_at(deq_time1 + Duration::from_millis(10)); // dequeues 10000 bytes
        assert!(queue.avg_drate > 0.0, "avg_drate should be calculated");
        assert!(
            queue.start_measurement.is_none(),
            "Should exit measurement cycle since now_bytes drops below threshold"
        );
    }

    #[test_log::test]
    fn test_pie_queue_update_drop_probability() {
        let config = PieQueueConfig::default();
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config.clone()).unwrap();

        // Fake high delay
        queue.avg_drate = 1000.0;
        queue.now_bytes = 100000; // cur_del = 100000 / 1000.0 = 100.0s > ref_del (0.015s)
        queue.old_del = 50.0; // old_del = 50.0s
        queue.p = 0.0; // start with p = 0

        // Expected p_increment calculation:
        // tilde_alpha = 0.125, tilde_beta = 1.25
        // p_increment = 0.125 * (100.0 - 0.015) + 1.25 * (100.0 - 50.0)
        // p_increment = 12.498125 + 62.5 = 74.998125
        // Since initial p = 0.0 < 0.000001, p_increment /= 2048.0
        // p_increment = 74.998125 / 2048.0 ≈ 0.036620178
        // Final p should be exactly this value
        let expected_p = (0.125 * (100.0 - config.ref_del) + 1.25 * (100.0 - 50.0)) / 2048.0;

        // Force next enqueue to trigger update_drop_probability() deterministically
        let mut pkt = create_packet(14);
        queue.start_update = Some(pkt.get_timestamp());
        pkt.delay_until(queue.start_update.unwrap() + Duration::from_millis(16));

        // This enqueue will trigger update_drop_probability()
        queue.enqueue(pkt);

        assert!(
            (queue.p - expected_p).abs() < f64::EPSILON,
            "Probability should be exactly calculated based on PIE formula. Expected: {}, Got: {}",
            expected_p,
            queue.p
        );
    }
}
