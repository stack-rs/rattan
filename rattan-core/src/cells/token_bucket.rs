use crate::cells::bandwidth::queue::PacketQueue;
use crate::cells::{Cell, Packet};
use crate::error::Error;
use crate::metal::timer::Timer;
use async_trait::async_trait;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::cmp::min;
use std::fmt::Debug;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tracing::{debug, info};

use super::bandwidth::queue::{DropTailQueue, DropTailQueueConfig};
use super::bandwidth::LARGE_DURATION;
use super::{ControlInterface, Egress, Ingress};

// transfer time into byte
fn time_to_length(time: Duration, token_bucket: TokenBucket) -> u64 {
    ((time.as_nanos() * token_bucket.rate as u128) / 1_000_000_000_u128) as u64
}
// transfer byte into time
fn length_to_time(length: usize, token_bucket: TokenBucket) -> Duration {
    if length == 0 {
        Duration::from_secs(0)
    } else if token_bucket.rate == 0 {
        LARGE_DURATION
    } else {
        Duration::from_secs_f64(length as f64 / token_bucket.rate as f64)
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone)]
pub struct TokenBucketConfig {
    pub buffer: i64,
    pub rate: u64,
}

// if buffer == -1, the token bucket will not work
impl Default for TokenBucketConfig {
    fn default() -> Self {
        Self {
            buffer: -1,
            rate: 0,
        }
    }
}

impl TokenBucketConfig {
    pub fn new<A: Into<i64>, B: Into<u64>>(buffer: A, rate: B) -> Self {
        Self {
            buffer: buffer.into(),
            rate: rate.into(),
        }
    }
}

impl From<TokenBucketConfig> for TokenBucket {
    fn from(config: TokenBucketConfig) -> Self {
        TokenBucket::new(config.buffer, config.rate)
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone, Copy)]
pub struct TokenBucket {
    buffer: i64,
    rate: u64,
    tokens: i64,
}

// if buffer == -1, the token bucket will not work
impl Default for TokenBucket {
    fn default() -> Self {
        Self {
            buffer: -1,
            rate: 0,
            tokens: 0,
        }
    }
}

impl TokenBucket {
    pub fn new(buffer: i64, rate: u64) -> Self {
        Self {
            buffer,    // ms
            rate,      // byte/s
            tokens: 0, // ns
        }
    }
}

// max_size is number of bytes
fn calculate_max_size(token_bucket: TokenBucket, peak_token_bucket: Option<TokenBucket>) -> u32 {
    // calculate number of ns of buffer and mtu, and pick the min of them
    let token_bucket_size = Duration::from_millis(token_bucket.buffer as u64);
    if peak_token_bucket.is_none() {
        time_to_length(token_bucket_size, token_bucket) as u32
    } else {
        let peak_token_bucket = peak_token_bucket.unwrap();
        let peak_token_bucket_size = Duration::from_millis(peak_token_bucket.buffer as u64);

        min(
            time_to_length(token_bucket_size, token_bucket) as u32,
            time_to_length(peak_token_bucket_size, peak_token_bucket) as u32,
        )
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
    max_size: u32,
    token_bucket: TokenBucket,
    peak_token_bucket: Option<TokenBucket>,
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
    fn set_config(&mut self, config: TokenBucketCellConfig) {
        let now = Instant::now();
        if let Some(token_bucket) = config.token_bucket {
            info!(
                "Previous next_available distance: {:?}",
                self.next_available - now
            );
            info!("Previous check_point distance: {:?}", now - self.last_check);

            // update tokens
            let token_bucket = TokenBucket::from(token_bucket);
            let length = time_to_length(
                Duration::from_nanos(self.token_bucket.tokens as u64) + (now - self.last_check),
                self.token_bucket,
            );
            let mut toks = length_to_time(length as usize, token_bucket).as_nanos() as i64;
            let mut ptoks = 0;
            if self.peak_token_bucket.is_some() {
                let peak_token_bucket = self.peak_token_bucket.unwrap();
                ptoks = peak_token_bucket.tokens + (now - self.last_check).as_nanos() as i64;
                ptoks = if ptoks > peak_token_bucket.buffer * 1000000 {
                    peak_token_bucket.buffer * 1000000
                } else {
                    ptoks
                };
            }

            toks = if toks > token_bucket.buffer * 1000000 {
                token_bucket.buffer * 1000000
            } else {
                toks
            };

            // modify self.token_bucket
            self.last_check = now;
            self.token_bucket.buffer = token_bucket.buffer;
            self.token_bucket.rate = token_bucket.rate;
            self.token_bucket.tokens = toks;

            if self.peak_token_bucket.is_some() {
                let peak_token_bucket = self.peak_token_bucket.unwrap();
                self.peak_token_bucket = Some(TokenBucket {
                    buffer: peak_token_bucket.buffer,
                    rate: peak_token_bucket.rate,
                    tokens: ptoks,
                });
            }

            // calculate max_size
            self.max_size = calculate_max_size(self.token_bucket, self.peak_token_bucket);

            // check if packet can be delivered
            self.next_available = Instant::now();
        }
        if let Some(ptoken_bucket) = config.peak_token_bucket {
            if self.peak_token_bucket.is_some() {
                debug!(
                    "Previous next_available distance: {:?}",
                    self.next_available - now
                );
                debug!("Previous check_point distance: {:?}", now - self.last_check);
                // update tokens

                let peak_token_bucket = self.peak_token_bucket.unwrap();
                let mut toks = self.token_bucket.tokens + (now - self.last_check).as_nanos() as i64;
                let mut ptoks: i64;

                let ptoken_bucket = TokenBucket::from(ptoken_bucket);
                ptoks = length_to_time(
                    time_to_length(
                        Duration::from_nanos(peak_token_bucket.tokens as u64)
                            + (now - self.last_check),
                        peak_token_bucket,
                    ) as usize,
                    ptoken_bucket,
                )
                .as_nanos() as i64;
                ptoks = if ptoks > ptoken_bucket.buffer * 1000000 {
                    ptoken_bucket.buffer * 1000000
                } else {
                    ptoks
                };

                toks = if toks > self.token_bucket.buffer * 1000000 {
                    self.token_bucket.buffer * 1000000
                } else {
                    toks
                };

                // modify self.token_bucket.tokens
                self.last_check = now;
                self.token_bucket.tokens = toks;

                // modify self.peak_token_bucket
                self.peak_token_bucket = Some(TokenBucket {
                    buffer: ptoken_bucket.buffer,
                    rate: ptoken_bucket.rate,
                    tokens: ptoks,
                });

                // calculate max_size
                self.max_size = calculate_max_size(self.token_bucket, self.peak_token_bucket);

                // check if packet can be delivered
                self.next_available = Instant::now();
            }
        }
        if let Some(queue_config) = config.queue_config {
            debug!(?queue_config, "Set inner queue config:");
            self.packet_queue.configure(queue_config);
        }
    }

    // calculate and return next_available Instant
    fn calculate_next_available(&self, now: Instant, toks: i64, ptoks: i64) -> Instant {
        if self.peak_token_bucket.is_none() {
            if toks >= 0 {
                if now > self.next_available {
                    now
                } else {
                    self.next_available
                }
            } else {
                let wait_duration = Duration::from_nanos((-toks) as u64);
                now + wait_duration
            }
        } else {
            let tokens = if toks < ptoks { toks } else { ptoks };
            if tokens >= 0 {
                if now > self.next_available {
                    now
                } else {
                    self.next_available
                }
            } else {
                let wait_duration = Duration::from_nanos((-tokens) as u64);
                now + wait_duration
            }
        }
    }
}

#[async_trait]
impl<P> Egress<P> for TokenBucketCellEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // wait until next_available -> wait for packet and check it -> send packet or wait until next_available
        loop {
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
                                        if self.token_bucket.buffer == -1 {
                                            return Some(new_packet);
                                        }
                                        // check the size of packet and drop or enter the queue
                                        let packet_size = new_packet.l3_length() + self.packet_queue.get_extra_length();
                                        if packet_size > self.max_size as usize {
                                            // drop the packet
                                            return None;
                                        }
                                        else {
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
                    _ = self.timer.sleep(self.next_available - Instant::now()) => {
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
                                        if self.token_bucket.buffer == -1 {
                                            return Some(new_packet);
                                        }
                                        // check the size of packet and drop or enter the queue
                                        let packet_size = new_packet.l3_length() + self.packet_queue.get_extra_length();
                                        if packet_size > self.max_size as usize {
                                            // drop the packet
                                            return None;
                                        }
                                        else {
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

            // check if the packet is not larger than max_size
            if self.packet_queue.get_front_size().unwrap() > self.max_size as usize {
                return None;
            }

            // get number of tokens added from last_check
            let now = Instant::now();
            let mut toks = min(
                self.token_bucket.buffer * 1000000,
                (now - self.last_check).as_nanos() as i64,
            );
            let mut ptoks: i64 = 0;
            if self.peak_token_bucket.is_some() {
                let peak_token_bucket = self.peak_token_bucket.unwrap();
                ptoks = min(
                    toks + peak_token_bucket.tokens,
                    peak_token_bucket.buffer * 1000000,
                );
                ptoks -= length_to_time(
                    self.packet_queue.get_front_size().unwrap(),
                    peak_token_bucket,
                )
                .as_nanos() as i64;
            }
            toks = min(
                toks + self.token_bucket.tokens,
                self.token_bucket.buffer * 1000000,
            );
            toks -= length_to_time(
                self.packet_queue.get_front_size().unwrap(),
                self.token_bucket,
            )
            .as_nanos() as i64;

            // set next_available
            self.next_available = self.calculate_next_available(now, toks, ptoks);

            // if tokens and ptokens are all positive, send the packet, and update number of tokens
            if toks >= 0 && ptoks >= 0 {
                // update last_check
                self.last_check = now;
                let packet_to_send = self.packet_queue.dequeue().unwrap();
                self.token_bucket.tokens = toks;
                if self.peak_token_bucket.is_some() {
                    let mut peak_token_bucket = self.peak_token_bucket.unwrap();
                    peak_token_bucket.tokens = ptoks;
                    self.peak_token_bucket = Some(peak_token_bucket);
                }
                return Some(packet_to_send);
            }

            // fail to send packet, wait for next_available
        }
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
    pub token_bucket: Option<TokenBucketConfig>,
    pub peak_token_bucket: Option<TokenBucketConfig>,
    pub queue_config: Option<DropTailQueueConfig>,
}

impl Clone for TokenBucketCellConfig {
    fn clone(&self) -> Self {
        Self {
            token_bucket: self.token_bucket.clone(),
            peak_token_bucket: self.peak_token_bucket.clone(),
            queue_config: self.queue_config.clone(),
        }
    }
}

impl TokenBucketCellConfig {
    pub fn new<
        T: Into<Option<TokenBucketConfig>>,
        TP: Into<Option<TokenBucketConfig>>,
        U: Into<Option<DropTailQueueConfig>>,
    >(
        token_bucket: T,
        peak_token_bucket: TP,
        queue_config: U,
    ) -> Self {
        Self {
            token_bucket: token_bucket.into(),
            peak_token_bucket: peak_token_bucket.into(),
            queue_config: queue_config.into(),
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
        if config.token_bucket.is_none()
            && config.peak_token_bucket.is_none()
            && config.queue_config.is_none()
        {
            // This ensures that incorrect HTTP requests will return errors.
            return Err(Error::ConfigError(
                "At least one of token_bucket, peak_token_bucket and queue_config should be set"
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
    pub fn new<TB: Into<Option<TokenBucket>>, PTB: Into<Option<TokenBucket>>>(
        token_bucket: TB,
        peak_token_bucket: PTB,
        packet_queue: DropTailQueue<P>,
    ) -> Result<TokenBucketCell<P>, Error> {
        debug!("New TokenBucketCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        let token_bucket = token_bucket.into().unwrap_or_default(); // or_default - check the existence in config file
        let _peak_token_bucket = peak_token_bucket.into().unwrap_or_default();
        let peak_token_bucket = if _peak_token_bucket.buffer == -1 {
            None
        } else {
            Some(_peak_token_bucket)
        };
        debug!(
            "max_size: {}",
            calculate_max_size(token_bucket, peak_token_bucket)
        );
        Ok(TokenBucketCell {
            ingress: Arc::new(TokenBucketCellIngress { ingress: rx }),
            egress: TokenBucketCellEgress {
                egress: tx,
                max_size: calculate_max_size(token_bucket, peak_token_bucket),
                token_bucket,
                peak_token_bucket,
                packet_queue,
                next_available: Instant::now(),
                last_check: Instant::now(),
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

    #[test_log::test]
    fn test_token_bucket_cell_basic() -> Result<(), Error> {
        // test normal token bucket
        let _span = span!(Level::INFO, "test_token_bucket_cell_basic").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 1000 ms buffer, 512 B/s rate and a 1024 bytes limit queue.");
        let token_bucket = TokenBucket::new(1000, 512);
        let token_bucket_queue = DropTailQueue::new(DropTailQueueConfig::new(
            None,
            1024,
            crate::cells::bandwidth::BwType::NetworkLayer,
        ));

        let cell = TokenBucketCell::new(token_bucket, None, token_bucket_queue)?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Testing delay time for token bucket cell");
        let mut delays: Vec<f64> = Vec::new();

        for _ in 0..10 {
            let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            let start = Instant::now();
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            //info!("duration: {}", duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == 256);
        }

        info!("Tested delays for token bucket cell: {:?}", delays);
        let average_rate = 2420.0 / (delays.iter().sum::<f64>() / 1000.0);
        info!("Tested rate for token bucket cell: {}", average_rate);

        let average_delay = delays.iter().sum::<f64>() / 10.0;
        debug!("Delays: {:?}", delays);
        info!(
            "Average delay: {:.3}ms, error {:.1}ms",
            average_delay,
            (average_delay - 472.65625).abs()
        );
        // Check the delay time
        assert!((average_delay - 472.65625).abs() <= DELAY_ACCURACY_TOLERANCE);

        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_cell_prate_low() -> Result<(), Error> {
        // test prate when prate is lower than rate
        let _span = span!(Level::INFO, "test_token_bucket_cell_prate_low").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 1000 ms buffer, 1024 B/s rate and a 1024 bytes limit queue.");
        let token_bucket = TokenBucket::new(1000, 1024);
        info!("The cell has a 250 ms buffer, 512 B/s rate peak bucket.");
        let peak_token_bucket = TokenBucket::new(250, 512);
        let token_bucket_queue = DropTailQueue::new(DropTailQueueConfig::new(
            None,
            1024,
            crate::cells::bandwidth::BwType::NetworkLayer,
        ));

        let cell = TokenBucketCell::new(token_bucket, peak_token_bucket, token_bucket_queue)?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Testing buffer of token bucket cell");
        let mut delays: Vec<f64> = Vec::new();

        // Test buffer
        // wait for 1500ms
        std::thread::sleep(Duration::from_millis(1500));
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

            //info!("duration: {}", duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == 128);
        }

        info!("Tested delays for token bucket cell: {:?}", delays);
        let average_rate = 570.0 / (delays.iter().sum::<f64>() / 1000.0);
        info!("Tested rate for token bucket cell: {}", average_rate);

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
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 1000 ms buffer, 512 B/s rate and a 1024 bytes limit queue.");
        let token_bucket = TokenBucket::new(1000, 512);
        info!("The cell has a 250 ms buffer, 1024 B/s rate peak bucket.");
        let peak_token_bucket = TokenBucket::new(250, 1024);
        let token_bucket_queue = DropTailQueue::new(DropTailQueueConfig::new(
            None,
            1024,
            crate::cells::bandwidth::BwType::NetworkLayer,
        ));

        let cell = TokenBucketCell::new(token_bucket, peak_token_bucket, token_bucket_queue)?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Testing buffer of token bucket cell");
        let mut delays: Vec<f64> = Vec::new();

        // Test buffer
        // wait for 1500ms
        std::thread::sleep(Duration::from_millis(1500));
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

            //info!("duration: {}", duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == 128);
        }

        info!("Tested delays for token bucket cell: {:?}", delays);
        let average_rate = 570.0 / (delays.iter().sum::<f64>() / 1000.0);
        info!("Tested rate for token bucket cell: {}", average_rate);

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
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 1000 ms buffer, 512 B/s rate and a 1024 bytes limit queue.");
        let token_bucket = TokenBucket::new(1000, 512);
        let token_bucket_queue = DropTailQueue::new(DropTailQueueConfig::new(
            None,
            1024,
            crate::cells::bandwidth::BwType::NetworkLayer,
        ));

        let cell = TokenBucketCell::new(token_bucket, None, token_bucket_queue)?;
        let config_changer = cell.control_interface();
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        //Test delay before update
        let test_packet = StdPacket::from_raw_buffer(&[0; 256]);

        let start = Instant::now();
        ingress.enqueue(test_packet)?;
        let received = rt.block_on(async { egress.dequeue().await });
        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay before update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);

        assert!((duration - 472.65625).abs() <= DELAY_ACCURACY_TOLERANCE);

        // test whether the packet will wait longer if the config is updated
        let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
        let start = Instant::now();
        ingress.enqueue(test_packet)?;
        // Wait for 250ms, then change the config to let the delay be longer
        std::thread::sleep(Duration::from_millis(250));
        // buffer: 1000 ms, rate: 256 B/s
        let token_bucket = TokenBucketConfig::new(1000, 256_u64);
        let token_bucket_queue =
            DropTailQueueConfig::new(None, 1024, crate::cells::bandwidth::BwType::NetworkLayer);
        config_changer.set_config(TokenBucketCellConfig::new(
            Some(token_bucket),
            None,
            Some(token_bucket_queue),
        ))?;

        let received = rt.block_on(async { egress.dequeue().await });

        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay after update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);

        assert!((duration - 695.3125).abs() <= DELAY_ACCURACY_TOLERANCE);
        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_cell_config_update_up() -> Result<(), Error> {
        // test config update of token bucket cells
        // set rate to be higher
        let _span = span!(Level::INFO, "test_token_bucket_cell_config_update_up").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 1000 ms buffer, 256 B/s rate and a 1024 bytes limit queue.");
        let token_bucket = TokenBucket::new(1000, 256);
        let token_bucket_queue = DropTailQueue::new(DropTailQueueConfig::new(
            None,
            1024,
            crate::cells::bandwidth::BwType::NetworkLayer,
        ));

        let cell = TokenBucketCell::new(token_bucket, None, token_bucket_queue)?;
        let config_changer = cell.control_interface();
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        //Test delay before update
        let test_packet = StdPacket::from_raw_buffer(&[0; 256]);

        let start = Instant::now();
        ingress.enqueue(test_packet)?;
        let received = rt.block_on(async { egress.dequeue().await });
        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay before update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);

        assert!((duration - 945.3125).abs() <= DELAY_ACCURACY_TOLERANCE);
        // Test whether the packet will wait shorter if the config is updated
        let test_packet = StdPacket::from_raw_buffer(&[0; 256]);

        let start = Instant::now();
        ingress.enqueue(test_packet)?;

        // Wait for 500ms, then change the config to 1000ms, 512B/s
        std::thread::sleep(Duration::from_millis(500));
        // buffer: 1000 ms, rate: 512 B/s
        let token_bucket = TokenBucketConfig::new(1000, 512_u64);
        let token_bucket_queue =
            DropTailQueueConfig::new(None, 1024, crate::cells::bandwidth::BwType::NetworkLayer);
        config_changer.set_config(TokenBucketCellConfig::new(
            Some(token_bucket),
            None,
            Some(token_bucket_queue),
        ))?;

        let received = rt.block_on(async { egress.dequeue().await });

        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay after update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);

        assert!((duration - 722.65625).abs() <= DELAY_ACCURACY_TOLERANCE);

        Ok(())
    }

    #[test_log::test]
    fn test_token_bucket_cell_queue() -> Result<(), Error> {
        // check dropped packet and sequence of packet, and max_size
        let _span = span!(Level::INFO, "test_token_bucket_cell_queue").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with 1000 ms buffer, 512 B/s rate and a 1024 bytes limit queue.");
        let token_bucket = TokenBucket::new(1000, 512);
        let token_bucket_queue = DropTailQueue::new(DropTailQueueConfig::new(
            None,
            1024,
            crate::cells::bandwidth::BwType::NetworkLayer,
        ));

        let cell = TokenBucketCell::new(token_bucket, None, token_bucket_queue)?;
        let mut received_packets: Vec<bool> = vec![false, false, false, false, false];
        let ingress = cell.sender();
        // let config_changer = cell.control_interface();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Testing queue of token bucket cell");
        let mut delays: Vec<f64> = Vec::new();

        // Test max_size
        let test_packet = StdPacket::from_raw_buffer(&[0; 1024]);
        ingress.enqueue(test_packet)?;
        let received = rt.block_on(async { egress.dequeue().await });
        assert!(received.is_none());
        //info!("first packet: {}", (Instant::now() - start).as_millis());

        // Test queue
        // enqueue 5 packets of 256B
        for i in 0..5 {
            let mut test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            test_packet.set_flow_id(i);
            ingress.enqueue(test_packet)?;
            std::thread::sleep(Duration::from_millis(100));
        }
        for _ in 0..4 {
            let start = Instant::now();
            let received = rt.block_on(async { egress.dequeue().await });

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            //info!("duration: {}", duration);

            assert!(received.is_some());
            let received = received.unwrap();

            // The length should be correct
            received_packets[received.get_flow_id() as usize] = true;
            assert!(received.length() == 256);
        }

        // receive the first packet immediately
        assert!((delays[0]).abs() <= DELAY_ACCURACY_TOLERANCE);
        // get left 3 packets
        assert!((delays[1] - 445.3125).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[2] - 472.65625).abs() <= DELAY_ACCURACY_TOLERANCE);
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

        info!("Creating cell with 1000 ms buffer, 512 B/s rate and a 1024 bytes limit queue.");
        let token_bucket = TokenBucket::new(1000, 512);
        let token_bucket_queue = DropTailQueue::new(DropTailQueueConfig::new(
            None,
            1024,
            crate::cells::bandwidth::BwType::NetworkLayer,
        ));

        let cell = TokenBucketCell::new(token_bucket, None, token_bucket_queue)?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Testing buffer of token bucket cell");
        let mut delays: Vec<f64> = Vec::new();

        // Test buffer
        // wait for 1500ms
        std::thread::sleep(Duration::from_millis(1500));
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

            //info!("duration: {}", duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == 128);
        }

        info!("Tested delays for token bucket cell: {:?}", delays);
        let average_rate = 570.0 / (delays.iter().sum::<f64>() / 1000.0);
        info!("Tested rate for token bucket cell: {}", average_rate);

        assert!(delays[0] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[1] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[2] <= DELAY_ACCURACY_TOLERANCE);
        assert!(delays[3] <= DELAY_ACCURACY_TOLERANCE);
        assert!((delays[4] - 113.28125).abs() <= DELAY_ACCURACY_TOLERANCE);
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

        info!("Creating cell with 1000 ms buffer, 512 B/s rate and a 1024 bytes limit queue.");
        let token_bucket = TokenBucket::new(1000, 512);
        let token_bucket_queue = DropTailQueue::new(DropTailQueueConfig::new(
            None,
            1024,
            crate::cells::bandwidth::BwType::NetworkLayer,
        ));

        let cell = TokenBucketCell::new(token_bucket, None, token_bucket_queue)?;
        let mut received_packets: Vec<bool> = vec![false, false, false, false, false];
        let ingress = cell.sender();
        let config_changer = cell.control_interface();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Testing max_size of token bucket cell");

        // Test max_size
        let test_packet = StdPacket::from_raw_buffer(&[0; 1024]);
        ingress.enqueue(test_packet)?;
        let received = rt.block_on(async { egress.dequeue().await });
        assert!(received.is_none());

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
            std::thread::sleep(Duration::from_millis(100));
        }
        let token_bucket = TokenBucketConfig::new(1000, 128_u64);
        config_changer.set_config(TokenBucketCellConfig::new(Some(token_bucket), None, None))?;
        // the first packet can be received but the left are lost
        for _ in 0..5 {
            let received = rt.block_on(async { egress.dequeue().await });
            if received.is_some() {
                let received = received.unwrap();
                assert!(received.get_flow_id() == 0);
                received_packets[received.get_flow_id() as usize] = true;
                // The length should be correct
                assert!(received.length() == 128);
            }
        }
        assert!(received_packets[0]);
        assert!(!received_packets[1]);
        assert!(!received_packets[2]);
        assert!(!received_packets[3]);
        assert!(!received_packets[4]);
        Ok(())
    }
}
