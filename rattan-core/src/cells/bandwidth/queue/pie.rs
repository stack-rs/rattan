// PIE Queue Implementation Reference:
// https://www.rfc-editor.org/info/rfc8033
// https://ieeexplore.ieee.org/document/6602305
// Reproduced according to RFC 8033 Appendix B,
// rather than original paper or RFC Appendix A.

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
    #[cfg_attr(
        feature = "serde",
        serde(default = "default_pie_seed", skip_serializing_if = "serde_default")
    )]
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
