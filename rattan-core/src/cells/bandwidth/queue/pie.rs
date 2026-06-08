// PIE Queue Implementation Reference:
// https://www.rfc-editor.org/info/rfc8033
// https://ieeexplore.ieee.org/document/6602305
// Reproduced according to RFC 8033 Appendix B, 
// rather than original paper or RFC Appendix A.

use std::collections::VecDeque;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, Duration};
use rand::random_range;
use tracing::debug;

#[cfg(feature = "serde")]
use super::serde_default;
use super::{BwType, PacketQueue};
use crate::cells::Packet;

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(default))]
#[derive(Debug, Clone)]
pub struct PieQueueConfig {
    pub packet_limit: Option<usize>,
    pub byte_limit: Option<usize>,
    pub ref_del: f64, // target delay (sec)
    pub t_update: Duration, // update interval
    pub tilde_alpha: f64, // base value of alpha (Hz, 1/sec)
    pub tilde_beta: f64, // base value of beta (Hz, 1/sec)
    pub dq_threshold: usize, // threshold of queue length (bytes)
    pub epsilon: f64, // EWMA weight
    pub max_burst: f64, // MAX_BURST (ms)
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "serde_default"))]
    pub bw_type: BwType,
}

impl Default for PieQueueConfig {
    fn default() -> Self {
        Self {
            packet_limit: None,
            byte_limit: None,
            ref_del: 0.015, // RFC 8033
            t_update: Duration::from_millis(15),
            tilde_alpha: 0.125,
            tilde_beta: 1.25,
            dq_threshold: 16384, // 16 KB
            epsilon: 0.125,
            max_burst: 150.0,
            bw_type: BwType::default(),
        }
    }
}

impl PieQueueConfig {
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
        ref_del: f64,
        t_update: Duration,
        tilde_alpha: f64,
        tilde_beta: f64,
        dq_threshold: usize,
        epsilon: f64,
        max_burst: f64,
        bw_type: BwType
    ) -> Self {
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
            ref_del,
            t_update,
            tilde_alpha,
            tilde_beta,
            dq_threshold,
            epsilon,
            max_burst,
            bw_type
        }
    }
}

impl<P> From<PieQueueConfig> for PieQueue<P> {
    fn from(config: PieQueueConfig) -> Self {
        PieQueue::new(config)
    }
}

#[derive(Debug)]
pub struct PieQueue<P> {
    queue: VecDeque<P>,
    config: PieQueueConfig,
    now_bytes: usize,
    old_del: f64, // previous delay (sec)
    p: f64, // current drop probability
    dq_count: usize, // departure count (bytes)
    start_update: Instant, // start time of t_update
    start_measurement: Option<Instant>, // Some(Instant) when in a measurement cycle, None when quit
    avg_drate: f64, 
    burst_allowance: f64
}

impl<P> PieQueue<P> {
    pub fn new(config: PieQueueConfig) -> Self {
        debug!(?config, "New PieQueue");
        let max_burst = config.max_burst;
        Self {
            queue: VecDeque::new(),
            config,
            now_bytes: 0,
            old_del: 0.0,
            p: 0.0,
            dq_count: 0,
            start_update: Instant::now(),
            start_measurement: None,
            avg_drate: 0.0,
            burst_allowance: max_burst
        }
    }
}

impl<P> Default for PieQueue<P>
where 
    P: Packet
{
    fn default() -> Self {
        Self::new(PieQueueConfig::default())
    }
}

impl<P> PieQueue<P>
where
    P: Packet,
{
    fn update_drop_probability(&mut self) {
        let now = Instant::now();
        let elapsed_ms = now.saturating_duration_since(self.start_update).as_secs_f64() * 1000.0;
        
        let cur_del = if self.avg_drate.abs() < f64::EPSILON {
            0.0
        } else {
            self.now_bytes as f64 / self.avg_drate
        };

        let mut p_increment = self.config.tilde_alpha * (cur_del - self.config.ref_del)
            + self.config.tilde_beta * (cur_del - self.old_del);
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
        self.p += p_increment;
        // RFC 8033 Section 4.2: Exponential decay when system is not congested        
        if cur_del < self.config.ref_del / 2.0 && self.old_del < self.config.ref_del / 2.0 {
            self.p *= 0.98;
        } 
        self.p = self.p.clamp(0.0, 1.0);

        // RFC 8033 Section 4.4: Burst Tolerance
        if self.p < f64::EPSILON && cur_del < self.config.ref_del / 2.0 && self.old_del < self.config.ref_del / 2.0 {
            self.burst_allowance = self.config.max_burst;
        } else {
            self.burst_allowance = (self.burst_allowance - elapsed_ms).max(0.0);
        }
        self.old_del = cur_del;
        self.start_update = now;
    }

    fn should_drop(&self) -> bool {
        // RFC 8033 Section 4.4: Enqueue packet bypassing random drop if burst_allow > 0
        if self.burst_allowance > f64::EPSILON {
            return false;
        }

        // RFC 8033 Section 4.1: Bypass random drop logic to be work conserving
        let bypass_drop = (self.old_del < self.config.ref_del / 2.0 && self.p < 0.2)
            || self.queue.len() <= 2;
        if bypass_drop {
            return false;
        }

        let rand_val = random_range(0.0 .. 1.0);
        rand_val < self.p
    }

    fn update_avg_drate(&mut self, pkt_size: usize) {
        let now = Instant::now();

        // Enter a measurement cycle
        if self.now_bytes > self.config.dq_threshold && self.start_measurement.is_none() {
            self.start_measurement = Some(now);
            self.dq_count = 0;
        }

        // Update departure rate if we are in a measurement cycle
        if let Some(start) = self.start_measurement {
            self.dq_count += pkt_size;
            if self.dq_count >= self.config.dq_threshold {
                let dq_int = now.saturating_duration_since(start).as_secs_f64();
                if dq_int > f64::EPSILON {
                    let dq_rate = self.dq_count as f64 / dq_int;
                    if self.avg_drate.abs() < f64::EPSILON {
                        self.avg_drate = dq_rate;
                    } else {
                        self.avg_drate = (1.0 - self.config.epsilon) * self.avg_drate + self.config.epsilon * dq_rate;
                    }
                    self.start_measurement = Some(now);
                    self.dq_count = 0;
                }
            }

            // Exit measurement cycle if queue length drops below threshold
            if self.now_bytes < self.config.dq_threshold {
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

    fn configure(&mut self, config: Self::Config) {
        self.config = config;
    }

    fn is_zero_buffer(&self) -> bool {
        self.config.packet_limit.is_some_and(|limit| limit == 0)
            || self.config.byte_limit.is_some_and(|limit| limit == 0)
    }

    fn enqueue(&mut self, packet: P) {
        // Simulate time-driven with event-driven approach
        let interval_update = Instant::now().saturating_duration_since(self.start_update);
        if interval_update >= self.config.t_update {
            self.update_drop_probability();
        } 
        
        // hard limit check
        if self
            .config
            .packet_limit
            .is_none_or(|limit| self.queue.len() < limit)
            && self.config.byte_limit.is_none_or(|limit| {
                self.now_bytes + packet.l3_length() + self.config.bw_type.extra_length() <= limit
            })
        {
            match self.should_drop() {
                true => {
                    #[cfg(test)]
                    tracing::trace!(
                        p = self.p,
                        old_delay = self.old_del,
                        header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                        "Drop packet(l3_len: {}, extra_len: {}) due to PIE algorithm", packet.l3_length(), self.get_extra_length()
                    );
                    return;
                },
                false => {
                    self.now_bytes += packet.l3_length() + self.get_extra_length();
                    self.queue.push_back(packet);
                }
            }
        } else {
            #[cfg(test)]
            tracing::trace!(
                queue_len = self.queue.len(),
                now_bytes = self.now_bytes,
                header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                "Drop packet(l3_len: {}, extra_len: {}) due to hard limit", packet.l3_length(), self.config.bw_type.extra_length()
            );
        }
    }

    fn dequeue(&mut self) -> Option<P> {
        // Simulate time-driven with event-driven approach
        let interval_update = Instant::now().saturating_duration_since(self.start_update);
        if interval_update >= self.config.t_update {
            self.update_drop_probability();
        }

        match self.queue.pop_front() {
            Some(packet) => {
                let pkt_size = packet.l3_length() + self.get_extra_length();
                self.now_bytes -= pkt_size;
                self.update_avg_drate(pkt_size);
                Some(packet)
            },
            None => None,
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
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config);

        assert!(queue.is_empty());

        let pkt1 = create_packet(500);
        queue.enqueue(pkt1);
        assert!(!queue.is_empty());
        assert_eq!(queue.length(), 1);

        let dequeued = queue.dequeue();
        assert!(dequeued.is_some());
        assert!(queue.is_empty());
    }

    #[test_log::test]
    fn test_pie_queue_hard_limit_packet() {
        let mut config = PieQueueConfig::default();
        config.packet_limit = Some(2);
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config);

        queue.enqueue(create_packet(100));
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 2);

        // This one should be dropped due to packet limit
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 2);
    }

    #[test_log::test]
    fn test_pie_queue_hard_limit_byte() {
        let mut config = PieQueueConfig::default();
        config.byte_limit = Some(150);
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config);

        queue.enqueue(create_packet(100)); // l3 length 86.
        assert_eq!(queue.length(), 1);

        // This one should be dropped due to byte limit (86 + 86 > 150)
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 1);
    }

    #[test_log::test]
    fn test_pie_queue_burst_allowance() {
        let config = PieQueueConfig::default();
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config);

        // Force a high drop probability
        queue.p = 1.0;
        queue.burst_allowance = 100.0;
        
        // burst_allowance > 0 bypasses random drop
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 1);

        // Fill queue > 2 to bypass work conserving logic later
        queue.enqueue(create_packet(100));
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 3);

        queue.burst_allowance = 0.0;
        // With burst_allowance = 0.0, queue > 2, and p = 1.0, it should drop
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 3);
    }

    #[test_log::test]
    fn test_pie_queue_work_conserving() {
        let config = PieQueueConfig::default();
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config.clone());

        queue.p = 1.0;
        queue.burst_allowance = 0.0;

        // bypass_drop handles queue.len() <= 2
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 1);
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 2);
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 3);

        // For the 4th element, queue.len() is 3, so it does not bypass based on length
        // Since p = 1.0, it drops
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 3);

        // Now test bypass_drop condition: old_del < ref_del/2 and p < 0.2
        queue.p = 0.15;
        queue.old_del = config.ref_del / 3.0;
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 4);
    }

    #[test_log::test]
    fn test_pie_queue_avg_drate_update() {
        let mut config = PieQueueConfig::default();
        config.dq_threshold = 50; // Small threshold
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config);

        queue.enqueue(create_packet(114)); // l3 length 100
        queue.enqueue(create_packet(114)); // l3 length 100
        assert_eq!(queue.now_bytes, 200);

        // First dequeue triggers start of measurement cycle
        assert!(queue.start_measurement.is_none());
        queue.dequeue(); // dequeues 100 bytes
        assert!(queue.start_measurement.is_some());
        assert_eq!(queue.now_bytes, 100);

        std::thread::sleep(Duration::from_millis(10));

        // Second dequeue triggers calculation of avg_drate
        queue.dequeue(); // dequeues 100 bytes
        assert!(queue.avg_drate > 0.0, "avg_drate should be calculated");
        assert!(queue.start_measurement.is_none(), "Should exit measurement cycle since queue is empty");
    }

    #[test_log::test]
    fn test_pie_queue_update_drop_probability() {
        let config = PieQueueConfig::default();
        let mut queue: PieQueue<StdPacket> = PieQueue::new(config.clone());

        // Fake high delay
        queue.avg_drate = 1000.0;
        queue.now_bytes = 100000; // delay = 100.0s > ref_del
        
        std::thread::sleep(config.t_update); // Wait to exceed t_update
        
        // This enqueue will trigger update_drop_probability()
        queue.enqueue(create_packet(14)); 
        
        assert!(queue.p > 0.0, "Probability should increase when delay is high");
    }
}