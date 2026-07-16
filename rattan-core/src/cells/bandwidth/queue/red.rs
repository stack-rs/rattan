// Combined RED Queue with Adaptive mode
// Reference:
//   RED:  https://ieeexplore.ieee.org/stamp/stamp.jsp?tp=&arnumber=251892
//   ARED: https://www.icir.org/floyd/papers/adaptiveRed.pdf
//   Kernel RED/ARED: https://github.com/torvalds/linux/blob/master/include/net/red.h

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
        }
    }
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
}

impl<P> From<RedQueueConfig> for RedQueue<P> {
    fn from(config: RedQueueConfig) -> Self {
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

impl<P> RedQueue<P> {
    pub fn new(config: RedQueueConfig) -> Self {
        if let Err(e) = config.validate() {
            panic!("RedQueue::new: {}", e);
        }
        debug!(?config, "New RedQueue");
        Self {
            queue: VecDeque::new(),
            config,
            now_bytes: 0,
            average_queue_length: 0.0,
            count_packet: -1,
            idle_start: None,
            latest_max_p_update: None,
            rng: StdRng::seed_from_u64(42),
        }
    }
}

impl<P> Default for RedQueue<P>
where
    P: Packet,
{
    fn default() -> Self {
        Self::new(RedQueueConfig::default())
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

    fn configure(&mut self, config: Self::Config) {
        if let Err(e) = config.validate() {
            warn!("RedQueue: discard invalid configure: {}", e);
            return;
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
                self.latest_max_p_update = Some(now);
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
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

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
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

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
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

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
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

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
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

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
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

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
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

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
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

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
