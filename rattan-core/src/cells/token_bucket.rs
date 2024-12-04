use crate::cells::bandwidth::queue::PacketQueue;
use crate::cells::{Cell, Packet};
use crate::core::CALIBRATED_START_INSTANT;
use crate::error::Error;
use crate::metal::timer::Timer;
use async_trait::async_trait;
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

pub const LARGE_RATE_LIMIT: u64 = 1073741824;
pub const LARGE_BUFFER: usize = 1073741824;

// transfer byte into time
fn length_to_time(length: usize, rate: u64) -> Duration {
    if length == 0 {
        Duration::from_secs(0)
    } else if rate == 0 {
        LARGE_DURATION
    } else {
        Duration::from_secs_f64(length as f64 / rate as f64)
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone, Copy)]
pub struct TokenBucket {
    buffer: usize,
    rate: u64,
    tokens: i64,
}

impl Default for TokenBucket {
    fn default() -> Self {
        Self {
            buffer: LARGE_BUFFER,
            rate: LARGE_RATE_LIMIT,
            tokens: 0,
        }
    }
}

impl TokenBucket {
    pub fn new(buffer: usize, rate: u64) -> Self {
        Self {
            buffer,    // byte
            rate,      // byte/s
            tokens: 0, // ns
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
    max_size: usize,
    token_bucket: TokenBucket,
    peak_token_bucket: TokenBucket,
    packet_queue: DropTailQueue<P>,
    next_check: Instant,
    last_check: Instant,
    config_rx: mpsc::UnboundedReceiver<TokenBucketCellConfig>,
    timer: Timer,
    state: AtomicI32,
}

impl<P> TokenBucketCellEgress<P>
where
    P: Packet + Send + Sync,
{
    fn get_new_tb(&self, new_rate: Option<u64>, new_burst: Option<usize>) -> TokenBucket {
        let mut new_tb = match (new_rate, new_burst) {
            (None, None) => self.token_bucket,
            (Some(rate), None) => TokenBucket::new(self.token_bucket.buffer, rate),
            (None, Some(burst)) => TokenBucket::new(burst, self.token_bucket.rate),
            (Some(rate), Some(burst)) => TokenBucket::new(burst, rate),
        };
        new_tb.tokens = length_to_time(new_tb.buffer, new_tb.rate).as_nanos() as i64;
        new_tb
    }

    fn set_config(&mut self, config: TokenBucketCellConfig) {
        let now = Instant::now();
        debug!("Previous next_check distance: {:?}", self.next_check - now);
        debug!("Previous check_point distance: {:?}", now - self.last_check);

        // modify self.token_bucket
        self.last_check = now;

        self.token_bucket = self.get_new_tb(config.rate, config.burst);
        self.peak_token_bucket = self.get_new_tb(config.peakrate, config.mtu);

        debug!(
            "toks: {}, ptoks: {}",
            self.token_bucket.tokens, self.peak_token_bucket.tokens
        );

        // calculate max_size
        self.max_size = min(self.token_bucket.buffer, self.peak_token_bucket.buffer);

        // set queue
        if let Some(limit) = config.limit {
            debug!(?limit, "Set inner queue byte_limit: ");
            self.packet_queue.configure(DropTailQueueConfig::new(
                None,
                limit,
                crate::cells::bandwidth::BwType::NetworkLayer,
            ));
        }

        // check if all packets in the queue is smaller than max_size
        let len = self.packet_queue.length();
        for _ in 0..len {
            let front_size = self.packet_queue.get_front_size().unwrap();
            let packet = self.packet_queue.dequeue().unwrap();
            if front_size <= self.max_size {
                self.packet_queue.enqueue(packet);
            }
        }

        // check if packet can be delivered
        self.next_check = now;
    }

    // calculate and return next_check Instant
    fn calculate_next_check(&self, now: Instant, toks: i64, ptoks: i64) -> Instant {
        let tokens = min(toks, ptoks);
        if tokens >= 0 {
            max(now, self.next_check)
        } else {
            let wait_duration = Duration::from_nanos((-tokens) as u64);
            now + wait_duration
        }
    }
}

#[async_trait]
impl<P> Egress<P> for TokenBucketCellEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // wait until next_check -> wait for packet and check it -> send packet or wait until next_check
        loop {
            // wait until next_check
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
                                        // check the size of packet and drop or enter the queue
                                        let packet_size = new_packet.l3_length() + self.packet_queue.get_extra_length();
                                        if packet_size <= self.max_size {
                                            // enqueue the packet
                                            self.packet_queue.enqueue(new_packet);
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
                    _ = self.timer.sleep(self.next_check - Instant::now()) => {
                        break;
                    }
                }
            }

            // wait for packet to arrive and check it
            while self.packet_queue.is_empty() {
                // the queue is empty, wait for packets to arrive
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
                                        // check the size of packet and drop or enter the queue
                                        let packet_size = new_packet.l3_length() + self.packet_queue.get_extra_length();
                                        if packet_size <= self.max_size {
                                            // enqueue the packet
                                            self.packet_queue.enqueue(new_packet);
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
                }
            }
            // packet arrived, check the size of packet and number of tokens

            // get number of tokens added from last_check
            let now = Instant::now();
            let buffer_ns =
                length_to_time(self.token_bucket.buffer, self.token_bucket.rate).as_nanos() as i64;
            let mtu_ns = length_to_time(self.peak_token_bucket.buffer, self.peak_token_bucket.rate)
                .as_nanos() as i64;

            let mut toks = min(buffer_ns, (now - self.last_check).as_nanos() as i64);

            let mut ptoks: i64 = min(mtu_ns, toks + self.peak_token_bucket.tokens);

            ptoks -= length_to_time(
                self.packet_queue.get_front_size().unwrap(),
                self.peak_token_bucket.rate,
            )
            .as_nanos() as i64;

            toks = min(buffer_ns, toks + self.token_bucket.tokens);

            toks -= length_to_time(
                self.packet_queue.get_front_size().unwrap(),
                self.token_bucket.rate,
            )
            .as_nanos() as i64;

            // set next_check
            self.next_check = self.calculate_next_check(now, toks, ptoks);

            // if tokens and ptokens are all positive, send the packet, and update number of tokens
            if toks >= 0 && ptoks >= 0 {
                // update last_check
                self.last_check = now;
                let packet_to_send = self.packet_queue.dequeue().unwrap();
                self.token_bucket.tokens = toks;
                self.peak_token_bucket.tokens = ptoks;
                debug!(
                    "in dequeue, toks: {}, ptoks: {}",
                    self.token_bucket.tokens, self.peak_token_bucket.tokens
                );
                return Some(packet_to_send);
            }

            // fail to send packet, wait for next_check
        }
    }

    fn reset(&mut self) {
        let now = Instant::now();
        self.token_bucket.tokens =
            length_to_time(self.token_bucket.buffer, self.token_bucket.rate).as_nanos() as i64;
        self.peak_token_bucket.tokens =
            length_to_time(self.peak_token_bucket.buffer, self.peak_token_bucket.rate).as_nanos()
                as i64;
        self.next_check = *CALIBRATED_START_INSTANT.get_or_init(|| now);
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
#[derive(Debug)]
pub struct TokenBucketCellConfig {
    pub limit: Option<usize>,  // queue limit in bytes
    pub rate: Option<u64>,     // rate of normal bucket
    pub burst: Option<usize>,  // buffer in bytes
    pub peakrate: Option<u64>, // peak rate
    pub mtu: Option<usize>,    // buffer of peak bucket in bytes
}

impl Clone for TokenBucketCellConfig {
    fn clone(&self) -> Self {
        Self {
            limit: self.limit,
            rate: self.rate,
            burst: self.burst,
            peakrate: self.peakrate,
            mtu: self.mtu,
        }
    }
}

impl TokenBucketCellConfig {
    pub fn new<
        L: Into<Option<usize>>,
        R: Into<Option<u64>>,
        B: Into<Option<usize>>,
        PR: Into<Option<u64>>,
        MT: Into<Option<usize>>,
    >(
        limit: L,
        rate: R,
        burst: B,
        peakrate: PR,
        mtu: MT,
    ) -> Self {
        Self {
            limit: limit.into(),
            rate: rate.into(),
            burst: burst.into(),
            peakrate: peakrate.into(),
            mtu: mtu.into(),
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
        if config.limit.is_none()
            && config.rate.is_none()
            && config.burst.is_none()
            && config.peakrate.is_none()
            && config.mtu.is_none()
        {
            // This ensures that incorrect HTTP requests will return errors.
            return Err(Error::ConfigError(
                "At least one of five parameters (limit, rate, burst, peakrate, mtu) should be set"
                    .to_string(),
            ));
        }
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

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
        R: Into<Option<u64>>,
        B: Into<Option<usize>>,
        PR: Into<Option<u64>>,
        MT: Into<Option<usize>>,
    >(
        limit: L,
        rate: R,
        burst: B,
        peakrate: PR,
        mtu: MT,
    ) -> Result<TokenBucketCell<P>, Error> {
        debug!("New TokenBucketCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        let mut token_bucket = TokenBucket::new(
            burst.into().unwrap_or(LARGE_BUFFER),
            rate.into().unwrap_or(LARGE_RATE_LIMIT),
        );
        token_bucket.tokens =
            length_to_time(token_bucket.buffer, token_bucket.rate).as_nanos() as i64;
        let mut peak_token_bucket = TokenBucket::new(
            mtu.into().unwrap_or(LARGE_BUFFER),
            peakrate.into().unwrap_or(LARGE_RATE_LIMIT),
        );
        peak_token_bucket.tokens =
            length_to_time(peak_token_bucket.buffer, peak_token_bucket.rate).as_nanos() as i64;
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
                max_size: min(token_bucket.buffer, peak_token_bucket.buffer),
                token_bucket,
                peak_token_bucket,
                packet_queue,
                next_check: now,
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

        // info!("Creating cell with {}B burst, {}B/s rate, {}B mtu, {}B/s peakrate and a {}B limit queue.", config.burst.unwrap_or(LARGE_BUFFER), config.rate.unwrap_or(LARGE_RATE_LIMIT), config.mtu.unwrap_or(LARGE_BUFFER), config.peakrate.unwrap_or(LARGE_RATE_LIMIT), config.limit.unwrap_or(LARGE_BUFFER));
        // let config = TokenBucketCellConfig::new(1024, 512, 512, None, None);
        let cell = TokenBucketCell::new(
            config.limit,
            config.rate,
            config.burst,
            config.peakrate,
            config.mtu,
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
            let received = rt.block_on(async { egress.dequeue().await });

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
            config.mtu,
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
            let received = rt.block_on(async { egress.dequeue().await });

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

        info!("Change rate to {}B/s.", updated_config.rate.unwrap());
        config_changer.set_config(updated_config)?;

        for _ in 0..number {
            let buf: &[u8] = &vec![0; packet_size];
            let test_packet = StdPacket::from_raw_buffer(buf);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..number {
            let start = Instant::now();
            let received = rt.block_on(async { egress.dequeue().await });

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
        info!("Creating cell with 512 B burst, 512 B/s rate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(1024, 512, 512, None, None);

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
        // test prate when prate is lower than rate
        let _span = span!(Level::INFO, "test_token_bucket_cell_prate_low").entered();
        info!("Creating cell with 1024 B burst, 1024 B/s rate, 128 B mtu, 512 B/s peakrate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(1024, 1024, 1024, 512, 128);

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
        // test prate when prate is higher than rate
        let _span = span!(Level::INFO, "test_token_bucket_cell_prate_high").entered();
        info!("Creating cell with 512 B burst, 512 B/s rate, 256 B mtu, 1024 B/s peakrate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(1024, 512, 512, 1024, 256);

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
        // set rate to be lower
        let _span = span!(Level::INFO, "test_token_bucket_cell_config_update_down").entered();
        info!("Creating cell with 512 B burst, 512 B/s rate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(1024, 512, 512, None, None);
        let updated_config = TokenBucketCellConfig::new(None, 256, None, None, None);

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
        // set rate to be higher
        let _span = span!(Level::INFO, "test_token_bucket_cell_config_update_up").entered();
        info!("Creating cell with 256 B burst, 256 B/s rate and a 1024 bytes limit queue.");
        let config = TokenBucketCellConfig::new(1024, 256, 256, None, None);
        let updated_config = TokenBucketCellConfig::new(None, 512, None, None, None);

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
        // check dropped packet and sequence of packet
        let _span = span!(Level::INFO, "test_token_bucket_cell_queue").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 512 B burst, 512 B/s rate and a 1024 bytes limit queue.");

        let cell = TokenBucketCell::new(1024, 512, 512, None, None)?;
        let mut received_packets: Vec<bool> = vec![false, false, false, false, false];
        let ingress = cell.sender();
        // let config_changer = cell.control_interface();
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
            //std::thread::sleep(Duration::from_millis(100));
        }
        for _ in 0..4 {
            let start = Instant::now();
            let received = rt.block_on(async { egress.dequeue().await });

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
    fn test_token_bucket_cell_buffer() -> Result<(), Error> {
        // check the buffer of token bucket
        let _span = span!(Level::INFO, "test_token_bucket_cell_buffer").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 512 B burst, 512 B/s rate and a 1024 bytes limit queue.");

        let cell = TokenBucketCell::new(1024, 512, 512, None, None)?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Testing buffer of token bucket cell");
        let mut delays: Vec<f64> = Vec::new();

        // Test buffer
        // enqueue 5 packets of 128B
        for _ in 0..5 {
            let test_packet = StdPacket::from_raw_buffer(&[0; 128]);
            ingress.enqueue(test_packet)?;
        }
        for _ in 0..5 {
            let start = Instant::now();
            let received = rt.block_on(async { egress.dequeue().await });

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
            let received = rt.block_on(async { egress.dequeue().await });

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

        info!("Creating cell with 512 B burst, 512 B/s rate and a 1024 bytes limit queue.");

        let cell = TokenBucketCell::new(1024, 512, 512, None, None)?;
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

        let received = rt.block_on(async { egress.dequeue().await });
        assert!(received.is_some());

        let received = received.unwrap();
        assert!(received.get_flow_id() == 1);
        assert!(received.length() == 512);

        config_changer.set_config(TokenBucketCellConfig::new(None, None, 1024, None, None))?;
        let test_packet = StdPacket::from_raw_buffer(&[0; 1024]);
        ingress.enqueue(test_packet)?;

        let received = rt.block_on(async { egress.dequeue().await });
        assert!(received.is_some());

        let received = received.unwrap();
        assert!(received.length() == 1024);

        // Test updated max_size
        // enqueue 5 packets of 256B
        for i in 0..5 {
            let mut test_packet;
            if i == 0 {
                test_packet = StdPacket::from_raw_buffer(&[0; 128]);
                test_packet.set_flow_id(i);
            } else {
                test_packet = StdPacket::from_raw_buffer(&[0; 256]);
                test_packet.set_flow_id(i);
            }
            ingress.enqueue(test_packet)?;
        }
        //let token_bucket = TokenBucketConfig::new(1000, 128_u64);
        config_changer.set_config(TokenBucketCellConfig::new(None, None, 128, None, None))?;
        // the first packet can be received but the left are lost
        let received = rt.block_on(async { egress.dequeue().await });
        if received.is_some() {
            let received = received.unwrap();
            assert!(received.get_flow_id() == 0);
            received_packets[received.get_flow_id() as usize] = true;
            // The length should be correct
            assert!(received.length() == 128);
        }
        assert!(received_packets[0]);
        assert!(!received_packets[1]);
        assert!(!received_packets[2]);
        assert!(!received_packets[3]);
        assert!(!received_packets[4]);
        Ok(())
    }
}
