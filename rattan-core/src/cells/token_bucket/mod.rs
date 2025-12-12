use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use bandwidth::Bandwidth;
use bytesize::ByteSize;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tracing::debug;

use super::bandwidth::queue::{DropTailQueue, DropTailQueueConfig};
use super::TRACE_START_INSTANT;
use super::{ControlInterface, Egress, Ingress};
use crate::cells::bandwidth::queue::PacketQueue;
use crate::cells::{AtomicCellState, Cell, CellState, Packet, LARGE_DURATION};
use crate::error::Error;
use crate::metal::timer::Timer;

mod bucket;

use bucket::TokenBucket;

pub struct TokenBucketCellIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for TokenBucketCellIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for TokenBucketCellIngress<P>
where
    P: Packet + Send,
{
    fn enqueue(&self, packet: P) -> Result<(), Error> {
        self.ingress
            .send(packet)
            .map_err(|_| Error::ChannelError("Data channel is closed.".to_string()))?;
        Ok(())
    }
}

fn debug_relative_time(t: Instant) -> Duration {
    t.duration_since(*TRACE_START_INSTANT.get_or_init(Instant::now))
}

pub struct TokenBucketCellEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<P>,
    token_bucket: Option<TokenBucket>,
    next_available: Instant,
    peak_token_bucket: Option<TokenBucket>,
    max_size: ByteSize,
    packet_queue: DropTailQueue<P>,
    config_rx: mpsc::UnboundedReceiver<TokenBucketCellConfig>,
    timer: Timer,
    state: AtomicCellState,
    notify_rx: Option<tokio::sync::broadcast::Receiver<crate::control::RattanNotify>>,
    started: bool,
    logical_clock: Instant,
}

impl<P> TokenBucketCellEgress<P>
where
    P: Packet + Send,
{
    fn set_config(&mut self, config: TokenBucketCellConfig, timestamp: Instant) {
        let mut new_max_size = ByteSize(u64::MAX);
        self.token_bucket =
            if let (Some(burst_size), Some(token_rate)) = (config.burst, config.rate) {
                new_max_size = new_max_size.min(burst_size);
                TokenBucket::new(burst_size, token_rate, timestamp - LARGE_DURATION).into()
            } else {
                None
            };

        self.peak_token_bucket =
            if let (Some(burst_size), Some(token_rate)) = (config.minburst, config.peakrate) {
                new_max_size = new_max_size.min(burst_size);
                TokenBucket::new(burst_size, token_rate, timestamp - LARGE_DURATION).into()
            } else {
                None
            };

        // calculate max_size
        let old_max_size = self.max_size;
        self.max_size = new_max_size;

        // set queue
        if let Some(limit) = config.limit {
            debug!(?limit, "Set inner queue byte_limit: ");
            self.packet_queue.configure(DropTailQueueConfig::new(
                None,
                limit,
                crate::cells::bandwidth::BwType::NetworkLayer,
            ));
        }

        // drop all packets larger than max_size in the queue when max_size is shrunk
        if self.max_size < old_max_size {
            let max_length =
                (self.max_size.0 as usize).saturating_sub(self.packet_queue.get_extra_length());
            self.packet_queue
                .retain(|packet| packet.l3_length() <= max_length);
        }

        // set next_available
        if !self.packet_queue.is_empty() {
            debug!("set_config: set next_available to now");
            self.next_available = timestamp;
        } else {
            self.next_available = timestamp + LARGE_DURATION;
        }
    }

    // Returns None only if both token_buckets is None
    fn next_token_ready(&self, size: ByteSize) -> Option<Instant> {
        self.token_bucket
            .iter()
            .chain(self.peak_token_bucket.iter())
            .map(|token_bucket| token_bucket.next_available(size))
            .max()
    }

    fn update_available(&mut self, current_size: Option<usize>) {
        if let Some(current_size) = current_size {
            let current_size = ByteSize(current_size as u64);
            let next_token_ready = self
                .next_token_ready(current_size)
                .expect("All token buckets are None");

            debug!(
                "Should wait until {:?}",
                debug_relative_time(next_token_ready)
            );
            self.next_available = next_token_ready;
        } else {
            debug!(
                "No packet to send at {:?}",
                debug_relative_time(self.logical_clock)
            );
            self.next_available += LARGE_DURATION;
        }
    }

    fn handle_packet(&mut self, new_packet: Option<P>) -> Option<P> {
        let front_size = self.packet_queue.get_front_size();
        let new_packet_size = new_packet
            .as_ref()
            .map(|p| self.packet_queue.get_packet_size(p));

        let packet = if let Some(current_bytes) = front_size.or(new_packet_size) {
            let current_size = ByteSize(current_bytes as u64);
            match (
                self.token_bucket.as_mut().map(|t| t.reserve(current_size)),
                self.peak_token_bucket
                    .as_mut()
                    .map(|t| t.reserve(current_size)),
            ) {
                (Some(None), _) | (_, Some(None)) => {
                    // We have packets waiting to be send, yet at least one token bucket is not ready
                    debug!(
                        "Insufficient token at {:?}",
                        debug_relative_time(self.logical_clock)
                    );
                    if let Some(new_packet) = new_packet {
                        self.packet_queue.enqueue(new_packet);
                    };
                    self.update_available(Some(current_bytes));
                    return None;
                }
                (Some(Some(token)), None) | (None, Some(Some(token))) => {
                    token.consume();
                }
                (Some(Some(token_a)), Some(Some(token_b))) => {
                    token_a.consume();
                    token_b.consume();
                }
                (None, None) => {}
            }
            // Send this packet!
            let mut current_packet = if let Some(head_packet) = self.packet_queue.dequeue() {
                if let Some(new_packet) = new_packet {
                    self.packet_queue.enqueue(new_packet);
                }
                head_packet.into()
            } else {
                // Send directly without enque
                new_packet
            }
            .expect("There should be a packet here");

            debug!(
                "Send packet, timestamp {:?} -> {:?}",
                debug_relative_time(current_packet.get_timestamp()),
                debug_relative_time(self.logical_clock)
            );
            current_packet.delay_until(self.logical_clock);
            current_packet.into()
        } else {
            None
        };
        let next_size = self.packet_queue.get_front_size();
        self.update_available(next_size);
        packet
    }

    fn update_timestamp(&mut self, timestamp: Instant) {
        let update = |tb: &mut TokenBucket| tb.update(timestamp);
        debug!(
            "[{:?}] Update timestamp to {:?}",
            debug_relative_time(Instant::now()),
            debug_relative_time(timestamp)
        );
        self.logical_clock = timestamp;
        self.token_bucket.iter_mut().for_each(update);
        self.peak_token_bucket.iter_mut().for_each(update);
    }
}

#[async_trait]
impl<P> Egress<P> for TokenBucketCellEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // Wait for Start notify if not started yet
        crate::wait_until_started!(self, Start);

        // wait until next_available
        let mut new_packet = None;
        while new_packet.is_none() {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config, Instant::now());
                }
                recv_packet = self.egress.recv() => {
                    // return None if the channel is closed
                    let recv_packet = recv_packet?;
                    self.update_timestamp(recv_packet.get_timestamp());

                    let recv_packet = crate::check_cell_state!(self.state, recv_packet);

                    // drop packet if is too large to pass through a full token bucket
                    let packet_size = self.packet_queue.get_packet_size(&recv_packet);
                    if packet_size > (self.max_size.0 as usize) {
                        continue;
                    }
                    new_packet =  self.handle_packet(recv_packet.into());
                }
                Ok(logical_now) = self.timer.sleep_until(self.next_available) => {
                    self.update_timestamp(logical_now);
                    new_packet =  self.handle_packet(None);
                }
            }
        }

        new_packet
    }

    fn reset(&mut self) {
        self.next_available = *TRACE_START_INSTANT.get_or_init(Instant::now);
    }

    fn change_state(&self, state: CellState) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }

    fn set_notify_receiver(
        &mut self,
        notify_rx: tokio::sync::broadcast::Receiver<crate::control::RattanNotify>,
    ) {
        self.notify_rx = Some(notify_rx);
    }
}

#[cfg_attr(
    feature = "serde",
    serde_with::skip_serializing_none,
    derive(Deserialize, Serialize)
)]
#[derive(Debug, Default)]
/// rate and burst, peakrate and minburst must be provided together
pub struct TokenBucketCellConfig {
    /// limit of the queue in bytes, default: unlimited
    #[cfg_attr(feature = "serde", serde(default))]
    pub limit: Option<usize>,
    #[cfg_attr(feature = "serde", serde(with = "human_bandwidth::serde", default))]
    /// rate of the bucket in bps, default: unlimited
    pub rate: Option<Bandwidth>,
    #[cfg_attr(feature = "serde", serde(with = "crate::utils::serde::byte", default))]
    /// burst of the bucket in bytes, default: unlimited
    pub burst: Option<ByteSize>,
    #[cfg_attr(feature = "serde", serde(with = "human_bandwidth::serde", default))]
    /// peakrate of the bucket in bps, default: unlimited
    pub peakrate: Option<Bandwidth>,
    #[cfg_attr(feature = "serde", serde(with = "crate::utils::serde::byte", default))]
    /// minburst of the bucket in bytes, default: unlimited
    pub minburst: Option<ByteSize>,
}

impl Clone for TokenBucketCellConfig {
    fn clone(&self) -> Self {
        Self {
            limit: self.limit,
            rate: self.rate,
            burst: self.burst,
            peakrate: self.peakrate,
            minburst: self.minburst,
        }
    }
}

impl TokenBucketCellConfig {
    pub fn new<
        L: Into<Option<usize>>,
        R: Into<Option<Bandwidth>>,
        B: Into<Option<ByteSize>>,
        PR: Into<Option<Bandwidth>>,
        MT: Into<Option<ByteSize>>,
    >(
        limit: L,
        rate: R,
        burst: B,
        peakrate: PR,
        minburst: MT,
    ) -> Self {
        Self {
            limit: limit.into(),
            rate: rate.into(),
            burst: burst.into(),
            peakrate: peakrate.into(),
            minburst: minburst.into(),
        }
    }
}

pub struct TokenBucketCellControlInterface {
    config_tx: mpsc::UnboundedSender<TokenBucketCellConfig>,
}

impl ControlInterface for TokenBucketCellControlInterface {
    type Config = TokenBucketCellConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        // set_config of TokenBucketCellControlInterface (send config through tx)
        if (config.rate.is_none() && config.burst.is_some())
            || (config.rate.is_some() && config.burst.is_none())
        {
            return Err(Error::ConfigError(
                "rate and burst should be provided together".to_string(),
            ));
        }
        if (config.peakrate.is_none() && config.minburst.is_some())
            || (config.peakrate.is_some() && config.minburst.is_none())
        {
            return Err(Error::ConfigError(
                "peakrate and minburst should be provided together".to_string(),
            ));
        }
        if let Some(rate) = config.rate {
            if rate.as_bps() == 0 {
                return Err(Error::ConfigError(
                    "rate must be greater than 0".to_string(),
                ));
            }
        }
        if let Some(burst) = config.burst {
            if burst.as_u64() == 0 {
                return Err(Error::ConfigError(
                    "burst must be greater than 0".to_string(),
                ));
            }
        }
        if let Some(peakrate) = config.peakrate {
            if peakrate.as_bps() == 0 {
                return Err(Error::ConfigError(
                    "peakrate must be greater than 0".to_string(),
                ));
            }
        }
        if let Some(minburst) = config.minburst {
            if minburst.as_u64() == 0 {
                return Err(Error::ConfigError(
                    "minburst must be greater than 0".to_string(),
                ));
            }
        }
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

/// Packets whose size is larger than the smaller one of burst and minburst will be dropped before enqueueing.
/// If the configuration is modified, packets in the queue whose size is larger than the smaller one of
/// the new burst and the new minburst will be dropped immediately.
pub struct TokenBucketCell<P: Packet> {
    ingress: Arc<TokenBucketCellIngress<P>>,
    egress: TokenBucketCellEgress<P>,
    control_interface: Arc<TokenBucketCellControlInterface>,
}

impl<P> Cell<P> for TokenBucketCell<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = TokenBucketCellIngress<P>;
    type EgressType = TokenBucketCellEgress<P>;
    type ControlInterfaceType = TokenBucketCellControlInterface;

    fn sender(&self) -> Arc<Self::IngressType> {
        self.ingress.clone()
    }

    fn receiver(&mut self) -> &mut Self::EgressType {
        &mut self.egress
    }

    fn into_receiver(self) -> Self::EgressType {
        self.egress
    }

    fn control_interface(&self) -> Arc<Self::ControlInterfaceType> {
        self.control_interface.clone()
    }
}

impl<P> TokenBucketCell<P>
where
    P: Packet,
{
    pub fn new<
        L: Into<Option<usize>>,
        R: Into<Option<Bandwidth>>,
        B: Into<Option<ByteSize>>,
        PR: Into<Option<Bandwidth>>,
        MT: Into<Option<ByteSize>>,
    >(
        limit: L,
        rate: R,
        burst: B,
        peakrate: PR,
        minburst: MT,
    ) -> Result<TokenBucketCell<P>, Error> {
        debug!("New TokenBucketCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();

        let now = Instant::now();
        let mut egress = TokenBucketCellEgress {
            egress: tx,
            max_size: ByteSize::b(0),
            token_bucket: None,
            peak_token_bucket: None,
            packet_queue: DropTailQueue::default(),
            next_available: now + LARGE_DURATION,
            config_rx,
            timer: Timer::new()?,
            state: AtomicCellState::new(CellState::Drop),
            notify_rx: None,
            started: false,
            logical_clock: now,
        };

        egress.set_config(
            TokenBucketCellConfig::new(limit, rate, burst, peakrate, minburst),
            now,
        );

        Ok(TokenBucketCell {
            ingress: Arc::new(TokenBucketCellIngress { ingress: rx }),
            egress,
            control_interface: Arc::new(TokenBucketCellControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::Instant;
    use tracing::{info, span, Level};

    use crate::cells::{StdPacket, TestPacket};

    use super::*;

    // The tolerance of the accuracy of the delays, in ms
    const DELAY_ACCURACY_TOLERANCE: f64 = 1.0;

    fn get_diff_ns(t1: &Instant, t2: &Instant) -> i64 {
        (t1.duration_since(*t2).as_nanos() as i64) - (t2.duration_since(*t1).as_nanos() as i64)
    }

    fn compare_delays(expected: Vec<Duration>, measured: Vec<Duration>, logical: Vec<Duration>) {
        assert_eq!(expected.len(), measured.len());
        assert_eq!(expected.len(), logical.len());
        for (idx, expected_delay) in expected.iter().enumerate() {
            assert_eq!(expected_delay, &logical[idx]);
            // A larger torlerance is needed in paralleled testing
            assert!(
                expected_delay.saturating_sub(measured[idx]).as_secs_f64()
                    <= DELAY_ACCURACY_TOLERANCE * 2E-3
            );
            assert!(
                measured[idx].saturating_sub(*expected_delay).as_secs_f64()
                    <= DELAY_ACCURACY_TOLERANCE * 2E-3
            );
        }
    }

    fn get_delays(
        number: usize,
        packet_size: usize,
        config: TokenBucketCellConfig,
        send_interval: Duration,
    ) -> Result<(Vec<Duration>, Vec<Duration>), Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();
        let mut timer = Timer::new().unwrap();

        let cell = TokenBucketCell::new(
            config.limit,
            config.rate,
            config.burst,
            config.peakrate,
            config.minburst,
        )?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        let mut time_shifts: Vec<i64> = Vec::new();
        let mut logical_delays = Vec::new();
        let mut measured_delays = Vec::new();

        let mut logical_time = Instant::now() + Duration::from_millis(10);

        for _ in 0..number {
            let buf: &[u8] = &vec![0; packet_size];
            let test_packet = TestPacket::<StdPacket>::with_timestamp(buf, logical_time);

            rt.block_on(async {
                timer.sleep_until(logical_time).await.unwrap();
                ingress.enqueue(test_packet).unwrap();
            });
            let send_time = Instant::now();

            let mut received: Option<TestPacket<StdPacket>> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }
            let actual_receive_time = Instant::now();

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == packet_size);

            time_shifts.push(get_diff_ns(&received.get_timestamp(), &actual_receive_time));
            logical_time = received.get_timestamp() + send_interval;
            logical_delays.push(received.delay());

            measured_delays.push(actual_receive_time.duration_since(send_time));
        }
        info!(
            "Tested time_shifts for token bucket cell: {:?}",
            time_shifts
        );
        info!(
            "Tested measured delays for token bucket cell: {:?}",
            measured_delays
        );
        info!(
            "Tested logical delays for token bucket cell: {:?}",
            logical_delays
        );

        let avg_time_shift = time_shifts.iter().map(|x| x.abs()).sum::<i64>() as f64 * 1E-6
            / time_shifts.len() as f64;
        info!(
            "Average time shift of receive times from packet timestamps: {:?}ms",
            avg_time_shift
        );
        assert!(avg_time_shift <= DELAY_ACCURACY_TOLERANCE);

        Ok((measured_delays, logical_delays))
    }

    fn get_delays_with_update(
        number: usize,
        packet_size: usize,
        config: TokenBucketCellConfig,
        updated_config: TokenBucketCellConfig,
    ) -> Result<(Vec<Duration>, Vec<Duration>), Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();
        let mut timer = Timer::new().unwrap();

        let cell = TokenBucketCell::new(
            config.limit,
            config.rate,
            config.burst,
            config.peakrate,
            config.minburst,
        )?;
        let controller_interface = cell.control_interface.clone();
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        let mut time_shifts: Vec<i64> = Vec::new();
        let mut logical_delays = Vec::new();
        let mut measured_delays = Vec::new();

        let logical_time = Instant::now() + Duration::from_millis(10);

        rt.block_on(async {
            timer.sleep_until(logical_time).await.unwrap();
        });

        for _ in 0..number {
            let buf: &[u8] = &vec![0; packet_size];
            let test_packet = TestPacket::<StdPacket>::with_timestamp(buf, logical_time);
            ingress.enqueue(test_packet).unwrap();
        }

        let send_time = Instant::now();

        for _ in 0..number {
            let mut received: Option<TestPacket<StdPacket>> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }
            let actual_receive_time = Instant::now();

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == packet_size);

            time_shifts.push(get_diff_ns(&received.get_timestamp(), &actual_receive_time));
            logical_delays.push(received.delay());

            measured_delays.push(actual_receive_time.duration_since(send_time));
        }

        controller_interface.config_tx.send(updated_config).unwrap();

        let logical_time = Instant::now() + Duration::from_millis(10);

        rt.block_on(async {
            timer.sleep_until(logical_time).await.unwrap();
        });

        for _ in 0..number {
            let buf: &[u8] = &vec![0; packet_size];
            let test_packet = TestPacket::<StdPacket>::with_timestamp(buf, logical_time);
            ingress.enqueue(test_packet).unwrap();
        }

        let send_time = Instant::now();

        for _ in 0..number {
            let mut received: Option<TestPacket<StdPacket>> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }
            let actual_receive_time = Instant::now();

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == packet_size);

            time_shifts.push(get_diff_ns(&received.get_timestamp(), &actual_receive_time));
            logical_delays.push(received.delay());

            measured_delays.push(actual_receive_time.duration_since(send_time));
        }

        info!(
            "Tested time_shifts for token bucket cell: {:?}",
            time_shifts
        );
        info!(
            "Tested measured delays for token bucket cell: {:?}",
            measured_delays
        );
        info!(
            "Tested logical delays for token bucket cell: {:?}",
            logical_delays
        );

        let avg_time_shift = time_shifts.iter().map(|x| x.abs()).sum::<i64>() as f64 * 1E-6
            / time_shifts.len() as f64;
        info!(
            "Average time shift of receive times from packet timestamps: {:?}ms",
            avg_time_shift
        );
        assert!(avg_time_shift <= DELAY_ACCURACY_TOLERANCE);

        Ok((measured_delays, logical_delays))
    }

    #[test_log::test]
    fn test_token_bucket_cell_basic() -> Result<(), Error> {
        // test normal token bucket
        let _span = span!(Level::INFO, "test_token_bucket_cell_basic").entered();
        info!("Creating cell with 512 B burst, 4096 bps rate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(
            1024,
            Bandwidth::from_bps(4096),
            ByteSize::b(512), // L3 length
            None,
            None,
        );

        // L3 size should be 256 Bytes
        let (measured_delays, logical_delays) =
            get_delays(8, 256 + 14, config, Duration::from_millis(10))?;

        // Initially, the bucket contains tokens for 2 packets
        // Each packet consumed token that needs 500ms to refill
        // Each packet was sent 10ms after the last packet as logically received
        compare_delays(
            vec![
                Duration::ZERO,
                Duration::ZERO,
                Duration::from_millis(480),
                Duration::from_millis(490),
                Duration::from_millis(490),
                Duration::from_millis(490),
                Duration::from_millis(490),
                Duration::from_millis(490),
            ],
            measured_delays,
            logical_delays,
        );

        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_cell_prate_low() -> Result<(), Error> {
        // test peakrate when peakrate is lower than rate
        let _span = span!(Level::INFO, "test_token_bucket_cell_prate_low").entered();
        info!("Creating cell with 1024 B burst, 8192 bps rate, 128 B minburst, 4096 bps peakrate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(
            1024,
            Bandwidth::from_bps(8192),
            ByteSize::b(1024),
            Bandwidth::from_bps(4096),
            ByteSize::b(128),
        );

        // 128 Byte L3 length
        let (measured_delays, logical_delays) =
            get_delays(5, 128 + 14, config, Duration::from_millis(10))?;

        // In this test, only minburst and peakrate is the bottleneck
        //
        // Initially, the bucket contains tokens for 8 / 1 packets
        // Each packet consumed token that needs 31.25ms / 250ms to refill
        // Each packet was sent 10ms after the last packet as logically received
        compare_delays(
            vec![
                Duration::ZERO,
                Duration::from_millis(240),
                Duration::from_millis(240),
                Duration::from_millis(240),
                Duration::from_millis(240),
            ],
            measured_delays,
            logical_delays,
        );

        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_cell_prate_high() -> Result<(), Error> {
        // test peakrate when peakrate is higher than rate
        let _span = span!(Level::INFO, "test_token_bucket_cell_prate_high").entered();
        info!("Creating cell with 512 B burst, 4096 bps rate, 256 B minburst, 8192 bps peakrate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(
            1024,
            Bandwidth::from_bps(4096),
            ByteSize::b(512),
            Bandwidth::from_bps(8192),
            ByteSize::b(256),
        );

        let (measured_delays, logical_delays) =
            get_delays(5, 128 + 14, config, Duration::from_millis(10))?;
        // In this test, rate and minburst is the bottleneck
        //
        // Initially, the bucket contains tokens for 4 / 2 packets
        // Each packet consumed token that needs 125ms / 62.5ms to refill
        // Each packet was sent 10ms after the last packet as logically received
        compare_delays(
            vec![
                Duration::ZERO,
                Duration::ZERO,
                Duration::from_millis(105),
                Duration::from_millis(115),
                Duration::from_millis(115),
            ],
            measured_delays,
            logical_delays,
        );

        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_cell_config_update_down() -> Result<(), Error> {
        // test config update of token bucket cells
        // set a lower rate
        let _span = span!(Level::INFO, "test_token_bucket_cell_config_update_down").entered();
        info!("Creating cell with 512 B burst, 4096 bps rate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(
            1024,
            Bandwidth::from_bps(4096),
            ByteSize::b(512),
            None,
            None,
        );
        let updated_config = TokenBucketCellConfig::new(
            None,
            Bandwidth::from_bps(2048),
            ByteSize::b(512),
            None,
            None,
        );

        // 256 Bytes L3 length
        let (measured_delays, logical_delays) =
            get_delays_with_update(4, 256 + 14, config, updated_config)?;

        // In this test, rate and burst is the bottleneck
        //
        // Initially, the bucket contains tokens for 2 packets
        // Each packet consumed token that needs 500ms to refill before the config change.
        // Each packet consumed token that needs 1s to refill after the config change.
        compare_delays(
            vec![
                Duration::ZERO,
                Duration::ZERO,
                Duration::from_millis(500),
                Duration::from_millis(1000),
                Duration::ZERO,
                Duration::ZERO,
                Duration::from_millis(1000),
                Duration::from_millis(2000),
            ],
            measured_delays,
            logical_delays,
        );

        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_cell_config_update_up() -> Result<(), Error> {
        // test config update of token bucket cells
        // set a higher rate
        let _span = span!(Level::INFO, "test_token_bucket_cell_config_update_up").entered();
        info!("Creating cell with 256 B burst, 2048 bps rate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(
            1024,
            Bandwidth::from_bps(2048),
            ByteSize::b(256),
            None,
            None,
        );
        let updated_config = TokenBucketCellConfig::new(
            None,
            Bandwidth::from_bps(4096),
            ByteSize::b(256),
            None,
            None,
        );
        // 256 Bytes L3 length
        let (measured_delays, logical_delays) =
            get_delays_with_update(4, 256 + 14, config, updated_config)?;

        // In this test, rate and burst is the bottleneck
        //
        // Initially, the bucket contains tokens for 1 packets
        // Each packet consumed token that needs 1s to refill before the config change.
        // Each packet consumed token that needs 500ms to refill after the config change.
        compare_delays(
            vec![
                Duration::ZERO,
                Duration::from_millis(1000),
                Duration::from_millis(2000),
                Duration::from_millis(3000),
                Duration::ZERO,
                Duration::from_millis(500),
                Duration::from_millis(1000),
                Duration::from_millis(1500),
            ],
            measured_delays,
            logical_delays,
        );

        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_cell_queue() -> Result<(), Error> {
        // check dropped packet and packet sequence
        let _span = span!(Level::INFO, "test_token_bucket_cell_queue").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 512 B burst, 4096 bps rate and a 1024 bytes limit queue.");

        let cell = TokenBucketCell::new(
            1024,
            Bandwidth::from_bps(4096),
            ByteSize::b(512),
            None,
            None,
        )?;
        let mut received_packets: Vec<bool> = vec![false, false, false, false, false];
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        info!("Testing queue of token bucket cell");
        let mut delays: Vec<f64> = Vec::new();
        let mut logical_delays = Vec::new();

        // Test queue
        // enqueue 5 packets of 256B
        for i in 0..5 {
            let mut test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256]);
            test_packet.set_flow_id(i);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..4 {
            let start = Instant::now();
            let mut received: Option<TestPacket<StdPacket>> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            assert!(received.is_some());
            let received = received.unwrap();

            // The length should be correct
            received_packets[received.get_flow_id() as usize] = true;
            assert!(received.length() == 256);

            logical_delays.push(received.delay());
        }

        // receive the first two packets immediately
        assert!(delays[0] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[1] <= DELAY_ACCURACY_TOLERANCE);
        // receive left 2 packets
        assert!((delays[2] - 417.875).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[3] - 472.65625).abs() <= DELAY_ACCURACY_TOLERANCE);

        assert!(received_packets[0]);
        assert!(received_packets[1]);
        assert!(received_packets[2]);
        assert!(received_packets[3]);
        // the last packet is lost
        assert!(!received_packets[4]);
        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_cell_burst() -> Result<(), Error> {
        // check the burst of token bucket cell
        let _span = span!(Level::INFO, "test_token_bucket_cell_burst").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 512 B burst, 4096 bps rate and a 1024 bytes limit queue.");

        let cell = TokenBucketCell::new(
            1024,
            Bandwidth::from_bps(4096),
            ByteSize::b(512),
            None,
            None,
        )?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        info!("Testing burst of token bucket cell");
        let mut delays: Vec<Duration> = Vec::new();
        let mut logical_delays = Vec::new();

        // Test burst
        // enqueue 5 packets of 128B

        let burst_time = Instant::now();
        for _ in 0..5 {
            let test_packet = TestPacket::<StdPacket>::with_timestamp(&[0; 128], burst_time);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..5 {
            let mut received: Option<TestPacket<StdPacket>> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }
            delays.push(burst_time.elapsed());

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == 128);

            logical_delays.push(received.delay());
        }

        // wait for 1000ms
        std::thread::sleep(Duration::from_millis(1000));
        let burst_time = Instant::now();
        // enqueue 5 packets of 128B
        for _ in 0..5 {
            let test_packet = TestPacket::<StdPacket>::with_timestamp(&[0; 128], burst_time);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..5 {
            let mut received: Option<TestPacket<StdPacket>> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }

            delays.push(burst_time.elapsed());

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == 128);

            logical_delays.push(received.delay());
        }

        info!("Tested measured delays for token bucket cell: {:?}", delays);
        info!(
            "Tested logical delays for token bucket cell: {:?}",
            logical_delays
        );

        compare_delays(
            vec![
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::from_nanos(113_281_250),
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::from_nanos(113_281_250),
            ],
            delays,
            logical_delays,
        );

        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_cell_max_size() -> Result<(), Error> {
        // check max_size of token bucket cell and its update
        let _span = span!(Level::INFO, "test_token_bucket_cell_max_size").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 512 B burst, 4096 bps rate and a 1024 bytes limit queue.");

        let cell = TokenBucketCell::new(
            None,
            Bandwidth::from_bps(4096),
            ByteSize::b(512),
            None,
            None,
        )?;
        let mut received_packets: Vec<bool> = vec![false, false, false, false, false];
        let ingress = cell.sender();
        let config_changer = cell.control_interface();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        info!("Testing max_size of token bucket cell");

        // Test max_size
        let mut test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 1024]);
        test_packet.set_flow_id(0);
        ingress.enqueue(test_packet)?;
        let mut test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 512]);
        test_packet.set_flow_id(1);
        ingress.enqueue(test_packet)?;

        let mut received: Option<TestPacket<StdPacket>> = None;
        while received.is_none() {
            received = rt.block_on(async { egress.dequeue().await });
        }
        assert!(received.is_some());

        let received = received.unwrap();
        assert!(received.get_flow_id() == 1);
        assert!(received.length() == 512);

        config_changer.set_config(TokenBucketCellConfig::new(
            None,
            Bandwidth::from_bps(4096),
            ByteSize::b(1024),
            None,
            None,
        ))?;
        let test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 1024]);
        ingress.enqueue(test_packet)?;

        let mut received: Option<TestPacket<StdPacket>> = None;
        while received.is_none() {
            received = rt.block_on(async { egress.dequeue().await });
        }
        assert!(received.is_some());

        let received = received.unwrap();
        assert!(received.length() == 1024);

        // Test updated max_size
        // enqueue 4 packets of 256B, and then enqueue a packet of 128B
        for i in 0..5 {
            let mut test_packet;
            if i == 4 {
                test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 128]);
                test_packet.set_flow_id(i);
            } else {
                test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256]);
                test_packet.set_flow_id(i);
            }
            ingress.enqueue(test_packet)?;
        }

        // call dequeue to make these 5 packets to enter the queue
        let mut received: Option<TestPacket<StdPacket>> = None;
        while received.is_none() {
            received = rt.block_on(async { egress.dequeue().await });
        }
        // before changing max_size, we can receive a packet of 256B
        if received.is_some() {
            let received = received.unwrap();
            assert!(received.get_flow_id() == 0);
            received_packets[received.get_flow_id() as usize] = true;
            // The length should be correct
            assert!(received.length() == 256);
        }

        config_changer.set_config(TokenBucketCellConfig::new(
            None,
            Bandwidth::from_bps(4096),
            ByteSize::b(128),
            None,
            None,
        ))?;
        // the 5th packet which is 128B can be received, but the left are lost
        let mut received: Option<TestPacket<StdPacket>> = None;
        while received.is_none() {
            received = rt.block_on(async { egress.dequeue().await });
        }
        if received.is_some() {
            let received = received.unwrap();
            assert!(received.get_flow_id() == 4);
            received_packets[received.get_flow_id() as usize] = true;
            // The length should be correct
            assert!(received.length() == 128);
        }
        assert!(received_packets[0]);
        assert!(!received_packets[1]);
        assert!(!received_packets[2]);
        assert!(!received_packets[3]);
        assert!(received_packets[4]);
        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_traffic_policing() -> Result<(), Error> {
        // check dropped packet and packet sequence
        let _span = span!(Level::INFO, "test_token_bucket_traffic_policing").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 512 B burst, 4096 bps rate and a 0 limit queue.");

        let cell =
            TokenBucketCell::new(0, Bandwidth::from_bps(4096), ByteSize::b(512), None, None)?;
        let mut received_packets: Vec<bool> = vec![false; 6];
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        info!("Testing queue of token bucket cell");
        let mut delays: Vec<f64> = Vec::new();

        // Test queue
        // enqueue 3 packets of 256B
        for i in 0..3 {
            let mut test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256]);
            test_packet.set_flow_id(i);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..2 {
            let start = Instant::now();
            let mut received: Option<_> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            assert!(received.is_some());
            let received = received.unwrap();

            // The length should be correct
            received_packets[received.get_flow_id() as usize] = true;
            tracing::debug!("Received flow id {}", received.get_flow_id());
            assert!(received.length() == 256);
        }

        // wait for 1000ms
        std::thread::sleep(Duration::from_millis(1000));

        // Test queue
        // enqueue 3 packets of 256B
        for i in 3..6 {
            let mut test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256]);
            test_packet.set_flow_id(i);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..2 {
            let start = Instant::now();
            let mut received: Option<_> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            assert!(received.is_some());
            let received = received.unwrap();

            // The length should be correct
            received_packets[received.get_flow_id() as usize] = true;
            tracing::debug!("Received flow id {}", received.get_flow_id());
            assert!(received.length() == 256);
        }

        // receive the first two packets immediately
        assert!(delays[0] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[1] <= DELAY_ACCURACY_TOLERANCE);

        // receive left 2 packets immediately
        assert!(delays[2] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[3] <= DELAY_ACCURACY_TOLERANCE);

        assert!(received_packets[0]);
        assert!(received_packets[1]);
        assert!(!received_packets[2]);
        assert!(received_packets[3]);
        assert!(received_packets[4]);
        assert!(!received_packets[5]);
        Ok(())
    }
}
