use crate::cells::bandwidth::queue::PacketQueue;
use crate::cells::{Cell, Packet};
use crate::core::CALIBRATED_START_INSTANT;
use crate::error::Error;
use crate::metal::timer::Timer;
use async_trait::async_trait;
use bandwidth::Bandwidth;
use bytesize::ByteSize;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::cmp::{max, min};
use std::fmt::Debug;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tracing::debug;

use super::bandwidth::queue::{DropTailQueue, DropTailQueueConfig};
use super::bandwidth::LARGE_DURATION;
use super::{ControlInterface, Egress, Ingress};

/// transfer length in bytes into time consumed to send the packet
fn length_to_time(length: u64, rate: Bandwidth) -> Duration {
    debug!("{}", rate.as_bps());
    if rate.as_bps() == 0 {
        LARGE_DURATION
    } else {
        Duration::from_secs_f64((8 * length) as f64 / rate.as_bps() as f64)
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone, Copy)]
pub struct TokenBucket {
    /// burst or minburst of tb or ptb, in bytes
    burst: ByteSize,
    /// rate or peakrate of tb or ptb, in bps
    rate: Bandwidth,
    /// number of tokens, in ns
    tokens: Duration,
}

impl Default for TokenBucket {
    fn default() -> Self {
        Self {
            burst: ByteSize::b(u64::MAX / 8),
            rate: Bandwidth::from_bps(u64::MAX),
            tokens: Duration::ZERO,
        }
    }
}

impl TokenBucket {
    /// Create a TokenBucket with burst in bytes and rate in bps.
    pub fn new(burst: ByteSize, rate: Bandwidth) -> Self {
        Self {
            burst,
            rate,
            tokens: Duration::ZERO,
        }
    }
}

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
    fn enqueue(&self, mut packet: P) -> Result<(), Error> {
        packet.set_timestamp(Instant::now());
        self.ingress
            .send(packet)
            .map_err(|_| Error::ChannelError("Data channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct TokenBucketCellEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<P>,
    max_size: ByteSize,
    token_bucket: TokenBucket,
    peak_token_bucket: TokenBucket,
    packet_queue: DropTailQueue<P>,
    next_available: Instant,
    last_check: Instant,
    config_rx: mpsc::UnboundedReceiver<TokenBucketCellConfig>,
    timer: Timer,
    state: AtomicI32,
}

impl<P> TokenBucketCellEgress<P>
where
    P: Packet + Send + Sync,
{
    fn set_token_bucket(&mut self, new_rate: Option<Bandwidth>, new_burst: Option<ByteSize>) {
        if let Some(rate) = new_rate {
            self.token_bucket.rate = rate;
        }
        if let Some(burst) = new_burst {
            self.token_bucket.burst = burst;
        }
        self.token_bucket.tokens =
            length_to_time(self.token_bucket.burst.as_u64(), self.token_bucket.rate);
    }

    fn set_peak_token_bucket(
        &mut self,
        new_peakrate: Option<Bandwidth>,
        new_minburst: Option<ByteSize>,
    ) {
        if let Some(peakrate) = new_peakrate {
            self.peak_token_bucket.rate = peakrate;
        }
        if let Some(minburst) = new_minburst {
            self.peak_token_bucket.burst = minburst;
        }
        self.peak_token_bucket.tokens = length_to_time(
            self.peak_token_bucket.burst.as_u64(),
            self.peak_token_bucket.rate,
        );
    }

    fn set_config(&mut self, config: TokenBucketCellConfig) {
        let now = Instant::now();
        debug!(
            "set_config: Previous next_available distance: {:?}",
            self.next_available - now
        );
        debug!("Previous check_point distance: {:?}", now - self.last_check);

        self.last_check = now;

        // modify self.token_bucket
        self.set_token_bucket(config.rate, config.burst);
        self.set_peak_token_bucket(config.peakrate, config.minburst);

        debug!(
            "toks: {}, ptoks: {}",
            self.token_bucket.tokens.as_nanos(),
            self.peak_token_bucket.tokens.as_nanos()
        );

        // calculate max_size
        let old_max_size = self.max_size;
        self.max_size = min(self.token_bucket.burst, self.peak_token_bucket.burst);

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
            let extra_length = self.packet_queue.get_extra_length();
            self.packet_queue
                .retain(|packet| packet.l3_length() + extra_length <= self.max_size.0 as usize);
        }

        // set next_available
        if !self.packet_queue.is_empty() {
            debug!("set_config: set next_available to now");
            self.next_available = now;
        } else {
            self.next_available = now + LARGE_DURATION;
        }
    }
}

#[async_trait]
impl<P> Egress<P> for TokenBucketCellEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // wait until next_available
        loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                recv_packet = self.egress.recv() => {
                    match recv_packet {
                        Some(new_packet) => {
                            match self.state.load(std::sync::atomic::Ordering::Acquire) {
                                0 => {
                                    return None;
                                }
                                1 => {
                                    return Some(new_packet);
                                }
                                _ => {
                                    // check the size of packet and drop it or let it enter the queue
                                    let packet_size = new_packet.l3_length() + self.packet_queue.get_extra_length();
                                    if packet_size <= (self.max_size.0 as usize) {
                                        // enqueue the packet
                                        if self.packet_queue.is_empty() {
                                            self.packet_queue.enqueue(new_packet);
                                            break;
                                        } else {
                                            self.packet_queue.enqueue(new_packet);
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            // channel closed
                            return None;
                        }
                    }
                }
                _ = self.timer.sleep(self.next_available - Instant::now()) => {
                    break;
                }
            }
        }
        let now = Instant::now();
        if let Some(size) = self.packet_queue.get_front_size() {
            // There will be at least one packet in the queue
            // get number of tokens added from last_check
            debug!(
                "Previous next_available distance: {:?}",
                self.next_available - now
            );
            let burst_duration =
                length_to_time(self.token_bucket.burst.as_u64(), self.token_bucket.rate);
            let minburst_duration = length_to_time(
                self.peak_token_bucket.burst.as_u64(),
                self.peak_token_bucket.rate,
            );
            self.token_bucket.tokens = min(
                burst_duration,
                now - self.last_check + self.token_bucket.tokens,
            );
            self.peak_token_bucket.tokens = min(
                minburst_duration,
                now - self.last_check + self.peak_token_bucket.tokens,
            );

            debug!("size: {}", size);
            debug!("max_size: {}", self.max_size);
            let consumed_tokens = length_to_time(size as u64, self.token_bucket.rate);
            let consumed_ptokens = length_to_time(size as u64, self.peak_token_bucket.rate);

            self.last_check = now;

            // if tokens and ptokens are both enough, send the packet, and update number of tokens
            if self.token_bucket.tokens >= consumed_tokens
                && self.peak_token_bucket.tokens >= consumed_ptokens
            {
                let packet_to_send = self.packet_queue.dequeue().unwrap();
                self.token_bucket.tokens -= consumed_tokens;
                self.peak_token_bucket.tokens -= consumed_ptokens;
                debug!(
                    "in dequeue, toks: {}, ptoks: {}",
                    self.token_bucket.tokens.as_nanos(),
                    self.peak_token_bucket.tokens.as_nanos()
                );
                // set next_available
                if let Some(next_size) = self.packet_queue.get_front_size() {
                    debug!("next_size: {}", next_size);
                    self.next_available = now
                        + max(
                            length_to_time(next_size as u64, self.token_bucket.rate)
                                .saturating_sub(self.token_bucket.tokens),
                            length_to_time(next_size as u64, self.peak_token_bucket.rate)
                                .saturating_sub(self.peak_token_bucket.tokens),
                        );
                } else {
                    self.next_available = now + LARGE_DURATION;
                }
                Some(packet_to_send)
            } else {
                // set next_available
                self.next_available = now
                    + max(
                        consumed_tokens.saturating_sub(self.token_bucket.tokens),
                        consumed_ptokens.saturating_sub(self.peak_token_bucket.tokens),
                    );
                None
            }
        } else {
            self.next_available = now + LARGE_DURATION;
            None
        }
    }

    fn reset(&mut self) {
        let now = Instant::now();
        self.next_available = *CALIBRATED_START_INSTANT.get_or_init(|| now);
        self.last_check = *CALIBRATED_START_INSTANT.get_or_init(|| now);
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
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
        let mut token_bucket = TokenBucket::new(
            burst.into().unwrap_or(ByteSize::b(u64::MAX / 8)),
            rate.into().unwrap_or(Bandwidth::from_bps(u64::MAX)),
        );
        token_bucket.tokens = length_to_time(token_bucket.burst.as_u64(), token_bucket.rate);

        let mut peak_token_bucket = TokenBucket::new(
            minburst.into().unwrap_or(ByteSize::b(u64::MAX / 8)),
            peakrate.into().unwrap_or(Bandwidth::from_bps(u64::MAX)),
        );
        peak_token_bucket.tokens =
            length_to_time(peak_token_bucket.burst.as_u64(), peak_token_bucket.rate);

        let packet_queue = DropTailQueue::new(DropTailQueueConfig::new(
            None,
            limit,
            crate::cells::bandwidth::BwType::NetworkLayer,
        ));

        let now = Instant::now();
        Ok(TokenBucketCell {
            ingress: Arc::new(TokenBucketCellIngress { ingress: rx }),
            egress: TokenBucketCellEgress {
                egress: tx,
                max_size: min(token_bucket.burst, peak_token_bucket.burst),
                token_bucket,
                peak_token_bucket,
                packet_queue,
                next_available: now + LARGE_DURATION,
                last_check: now,
                config_rx,
                timer: Timer::new()?,
                state: AtomicI32::new(0),
            },
            control_interface: Arc::new(TokenBucketCellControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;
    use tracing::{info, span, Level};

    use crate::cells::StdPacket;

    use super::*;

    // The tolerance of the accuracy of the delays, in ms
    const DELAY_ACCURACY_TOLERANCE: f64 = 1.0;

    fn get_delays(
        number: usize,
        packet_size: usize,
        config: TokenBucketCellConfig,
    ) -> Result<Vec<f64>, Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

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
        egress.change_state(2);

        info!("Testing delay time for token bucket cell");
        let mut delays: Vec<f64> = Vec::new();

        for _ in 0..number {
            let buf: &[u8] = &vec![0; packet_size];
            let test_packet = StdPacket::from_raw_buffer(buf);
            let start = Instant::now();
            ingress.enqueue(test_packet)?;
            let mut received: Option<StdPacket> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == packet_size);
        }
        info!("Tested delays for token bucket cell: {:?}", delays);

        Ok(delays)
    }

    fn get_delays_with_update(
        number: usize,
        packet_size: usize,
        config: TokenBucketCellConfig,
        updated_config: TokenBucketCellConfig,
    ) -> Result<Vec<f64>, Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        let cell = TokenBucketCell::new(
            config.limit,
            config.rate,
            config.burst,
            config.peakrate,
            config.minburst,
        )?;
        let config_changer = cell.control_interface();
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        let mut delays: Vec<f64> = Vec::new();

        for _ in 0..number {
            let buf: &[u8] = &vec![0; packet_size];
            let test_packet = StdPacket::from_raw_buffer(buf);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..number {
            let start = Instant::now();
            let mut received: Option<StdPacket> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == packet_size);
        }

        info!("Delays before update: {:?}", delays);

        info!(
            "Change rate to {}bps.",
            updated_config.rate.unwrap().as_bps()
        );
        config_changer.set_config(updated_config)?;

        for _ in 0..number {
            let buf: &[u8] = &vec![0; packet_size];
            let test_packet = StdPacket::from_raw_buffer(buf);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..number {
            let start = Instant::now();
            let mut received: Option<StdPacket> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == packet_size);
        }

        info!("Delays after update: {:?}", &delays[number..]);
        Ok(delays)
    }

    #[test_log::test]
    fn test_token_bucket_cell_basic() -> Result<(), Error> {
        // test normal token bucket
        let _span = span!(Level::INFO, "test_token_bucket_cell_basic").entered();
        info!("Creating cell with 512 B burst, 4096 bps rate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(
            1024,
            Bandwidth::from_bps(4096),
            ByteSize::b(512),
            None,
            None,
        );

        let delays = get_delays(8, 256, config)?;

        assert!(delays[0] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[1] <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[2] - 417.875).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[3] - 472.5625).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[4] - 472.5625).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[5] - 472.5625).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[6] - 472.5625).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[7] - 472.5625).abs() <= DELAY_ACCURACY_TOLERANCE);

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

        let delays = get_delays(5, 128, config)?;

        assert!(delays[0] <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[1] - 195.3125).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[2] - 222.65625).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[3] - 222.65625).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[4] - 222.65625).abs() <= DELAY_ACCURACY_TOLERANCE);

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

        let delays = get_delays(5, 128, config)?;

        assert!(delays[0] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[1] <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[2] - 83.984375).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[3] - 111.328125).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[4] - 111.328125).abs() <= DELAY_ACCURACY_TOLERANCE);
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

        let delays = get_delays_with_update(4, 256, config, updated_config)?;

        assert!(delays[0] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[1] <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[2] - 417.875).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[3] - 472.5625).abs() <= DELAY_ACCURACY_TOLERANCE);

        assert!(delays[4] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[5] <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[6] - 835.9375).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[7] - 945.3125).abs() <= DELAY_ACCURACY_TOLERANCE);
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

        let delays = get_delays_with_update(4, 256, config, updated_config)?;

        assert!(delays[0] <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[1] - 890.625).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[2] - 945.3125).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[3] - 945.3125).abs() <= DELAY_ACCURACY_TOLERANCE);

        assert!(delays[4] <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[5] - 445.3125).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[6] - 472.65625).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[7] - 472.65625).abs() <= DELAY_ACCURACY_TOLERANCE);
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
        egress.change_state(2);

        info!("Testing queue of token bucket cell");
        let mut delays: Vec<f64> = Vec::new();

        // Test queue
        // enqueue 5 packets of 256B
        for i in 0..5 {
            let mut test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            test_packet.set_flow_id(i);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..4 {
            let start = Instant::now();
            let mut received: Option<StdPacket> = None;
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
        egress.change_state(2);

        info!("Testing burst of token bucket cell");
        let mut delays: Vec<f64> = Vec::new();

        // Test burst
        // enqueue 5 packets of 128B
        for _ in 0..5 {
            let test_packet = StdPacket::from_raw_buffer(&[0; 128]);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..5 {
            let start = Instant::now();
            let mut received: Option<StdPacket> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == 128);
        }

        // wait for 1000ms
        std::thread::sleep(Duration::from_millis(1000));

        // enqueue 5 packets of 128B
        for _ in 0..5 {
            let test_packet = StdPacket::from_raw_buffer(&[0; 128]);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..5 {
            let start = Instant::now();
            let mut received: Option<StdPacket> = None;
            while received.is_none() {
                received = rt.block_on(async { egress.dequeue().await });
            }

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == 128);
        }

        info!("Tested delays for token bucket cell: {:?}", delays);

        assert!(delays[0] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[1] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[2] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[3] <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[4] - 113.28125).abs() <= DELAY_ACCURACY_TOLERANCE);

        assert!(delays[5] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[6] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[7] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[8] <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[9] - 113.28125).abs() <= DELAY_ACCURACY_TOLERANCE);
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
        egress.change_state(2);

        info!("Testing max_size of token bucket cell");

        // Test max_size
        let mut test_packet = StdPacket::from_raw_buffer(&[0; 1024]);
        test_packet.set_flow_id(0);
        ingress.enqueue(test_packet)?;
        let mut test_packet = StdPacket::from_raw_buffer(&[0; 512]);
        test_packet.set_flow_id(1);
        ingress.enqueue(test_packet)?;

        let mut received: Option<StdPacket> = None;
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
        let test_packet = StdPacket::from_raw_buffer(&[0; 1024]);
        ingress.enqueue(test_packet)?;

        let mut received: Option<StdPacket> = None;
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
                test_packet = StdPacket::from_raw_buffer(&[0; 128]);
                test_packet.set_flow_id(i);
            } else {
                test_packet = StdPacket::from_raw_buffer(&[0; 256]);
                test_packet.set_flow_id(i);
            }
            ingress.enqueue(test_packet)?;
        }

        // call dequeue to make these 5 packets to enter the queue
        let mut received: Option<StdPacket> = None;
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
        let mut received: Option<StdPacket> = None;
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
}
