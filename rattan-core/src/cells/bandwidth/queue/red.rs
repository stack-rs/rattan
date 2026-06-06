// RED Queue Implementation Reference:
// https://github.com/torvalds/linux/blob/master/include/net/red.h

use std::collections::VecDeque;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, Duration};
use rand::random_range;
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
    pub w_q: f64, // queue weight for calculating the average queue length
    pub min_th: usize, // minimum threshold of average queue length
    pub max_th: usize, // maximum threshold of average queue length
    pub max_p: f64, // maximum probability of dropping a packet
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "serde_default"))]
    pub pkt_tx_time: Duration, // typical packet tx time (us)
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "serde_default"))]
    pub bw_type: BwType,
}

impl Default for RedQueueConfig {
    fn default() -> Self {
        Self {
            packet_limit: None,
            byte_limit: None,
            w_q: 0.002,
            min_th: 7500, // 5 * 1500 bytes
            max_th: 22500, // 15 * 1500 bytes
            max_p: 0.02,
            pkt_tx_time: Duration::from_micros(120), // 1500 bytes * 8 / 100Mbps = 120 us
            bw_type: BwType::default(),
        }
    }
}

impl RedQueueConfig {
    pub fn new<A: Into<Option<usize>>, B: Into<Option<usize>>>(
        packet_limit: A,
        byte_limit: B,
        w_q: f64,
        min_th: usize,
        max_th: usize,
        max_p: f64,
        pkt_tx_time: Duration,
        bw_type: BwType
    ) -> Self {
        // Warning: The caller must ensure that the parameters are valid.
        // It's recommended to do validation before calling this function,
        // or we may need to return a Result instead of Self in the future.
        if min_th >= max_th {
            warn!("RedQueueConfig: min_th ({}) >= max_th ({}), which may cause invalid behavior.", min_th, max_th);
        }
        if pkt_tx_time.as_micros() == 0 {
            warn!("RedQueueConfig: pkt_tx_time is 0, which will cause divide-by-zero in m calculation.");
        }
        if !(0.0..=1.0).contains(&w_q) {
            warn!("RedQueueConfig: w_q ({}) is out of expected range [0.0, 1.0]. This is an EWMA weight.", w_q);
        }
        if !(0.0..=1.0).contains(&max_p) {
            warn!("RedQueueConfig: max_p ({}) is out of expected range [0.0, 1.0]. This is a probability.", max_p);
        }
        
        Self {
            packet_limit: packet_limit.into(),
            byte_limit: byte_limit.into(),
            w_q,
            min_th,
            max_th,
            max_p,
            pkt_tx_time,
            bw_type
        }
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
    now_bytes: usize, // for calculating average_queue_length
    average_queue_length: f64,
    count_packet: i32, // number of packets since last dropping
    idle_start: Option<Instant>, // start time of current idle period
}

impl<P> RedQueue<P> {
    pub fn new(config: RedQueueConfig) -> Self {
        debug!(?config, "New RedQueue");
        Self {
            queue: VecDeque::new(),
            config,
            now_bytes: 0,
            average_queue_length: 0.0,
            count_packet: -1,
            idle_start: None,
        }
    }
}

impl<P> Default for RedQueue<P>
where
    P: Packet
{
    fn default() -> Self {
        Self::new(RedQueueConfig::default())
    }
}

impl<P> RedQueue<P>
where
   P: Packet,
{
    fn update_avg (&mut self) {
        match self.is_empty() {
            false => {
                self.average_queue_length = (1.0 - self.config.w_q) * self.average_queue_length + self.config.w_q * (self.now_bytes as f64)
            },
            true => {
                if let Some(idle_start) = self.idle_start {
                    let now = Instant::now();
                    let idle_duration = now.duration_since(idle_start);
                    let m = idle_duration.as_micros() as f64 / self.config.pkt_tx_time.as_micros() as f64;
                    self.average_queue_length *= f64::powf(1.0 - self.config.w_q, m);
                    self.idle_start = Some(now);
                }
            }
        }
    }

    fn should_drop (&mut self) -> bool {
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

            let rand_val = random_range(0.0 .. 1.0);
            if rand_val < p_a {
                self.count_packet = 0;
                true
            } else {
                false
            }
        } else if avg >= max_th {
            self.count_packet = 0;
            true
        } else {
            self.count_packet = -1;
            false
        }
    }
}

impl<P> PacketQueue<P> for RedQueue<P>
where
    P: Packet,
{
    type Config = RedQueueConfig;

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
            let packet_size = packet.l3_length() + self.get_extra_length();
            self.update_avg();
            match self.should_drop() {
                true => {
                    #[cfg(test)]
                    tracing::trace!(
                        avg = self.average_queue_length,
                        count = self.count_packet,
                        header = ?format!("{:X?}", &packet.as_slice()[0..std::cmp::min(56, packet.length())]),
                        "Drop packet(l3_len: {}, extra_len: {}) due to RED algorithm", packet.l3_length(), self.get_extra_length()
                    );
                    return;
                },
                false => {
                    self.now_bytes += packet_size;
                    self.queue.push_back(packet);
                    self.idle_start = None;
                }
            }
        } else {
            self.count_packet = 0;
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
        match self.queue.pop_front() {
            Some(packet ) => {
                self.now_bytes -= packet.l3_length() + self.get_extra_length();
                if self.is_empty() {
                    self.idle_start = Some(Instant::now());
                }
                Some(packet)
            },
            None => None
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
    use crate::cells::{Packet, StdPacket};
    use tokio::time::Instant;

    fn create_packet(size: usize) -> StdPacket {
        let buf = vec![0u8; size];
        StdPacket::with_timestamp(&buf, Instant::now())
    }

    #[test_log::test]
    fn test_red_queue_basic() {
        let mut config = RedQueueConfig::default();
        config.min_th = 1000;
        config.max_th = 2000;
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

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
    fn test_red_queue_hard_limit_packet() {
        let mut config = RedQueueConfig::default();
        config.packet_limit = Some(2);
        config.min_th = 100000; // avoid red drop
        config.max_th = 200000;
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
        let mut config = RedQueueConfig::default();
        config.byte_limit = Some(150);
        config.min_th = 100000; // avoid red drop
        config.max_th = 200000;
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

        queue.enqueue(create_packet(100)); // l3 length 86.
        assert_eq!(queue.length(), 1);

        // This one should be dropped due to byte limit (86 + 86 > 150)
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), 1);
    }

    #[test_log::test]
    fn test_red_queue_max_th_drop() {
        let mut config = RedQueueConfig::default();
        config.min_th = 100;
        config.max_th = 200;
        config.w_q = 1.0; // max weight, avg matches instantly
        let mut queue: RedQueue<StdPacket> = RedQueue::new(config);

        // First packet
        queue.enqueue(create_packet(100)); 
        
        // Second packet
        queue.enqueue(create_packet(300));
        
        // At this point, queue length is 2, now_bytes is high enough.
        // The next enqueue should see average_queue_length > max_th and drop the packet.
        let before_len = queue.length();
        queue.enqueue(create_packet(100));
        assert_eq!(queue.length(), before_len, "Packet should be dropped by RED max_th");
    }

    #[test_log::test]
    fn test_red_queue_min_th_no_drop() {
        let mut config = RedQueueConfig::default();
        config.min_th = 1000;
        config.max_th = 2000;
        config.w_q = 1.0; // Instantly reach exact byte size
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
        let mut config = RedQueueConfig::default();
        config.min_th = 100;
        config.max_th = 300;
        config.max_p = 0.5;
        config.w_q = 1.0; 
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
        
        // It should drop some packets, but not all of them
        assert!(drop_count > 0, "Should have dropped some packets probabilistically");
        assert!(drop_count < total_packets, "Should not drop all packets");
    }
}