use crate::cells::{Cell, Packet};
use crate::core::CALIBRATED_START_INSTANT;
use crate::error::Error;
use crate::metal::timer::Timer;
use crate::utils::sync::AtomicRawCell;
use async_trait::async_trait;
use netem_trace::model::LossTraceConfig;
use netem_trace::{LossPattern, LossTrace};
use rand::Rng;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info};

use super::{ControlInterface, Egress, Ingress};

pub struct LossCellIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for LossCellIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for LossCellIngress<P>
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

pub struct LossCellEgress<P, R>
where
    P: Packet,
    R: Rng,
{
    egress: mpsc::UnboundedReceiver<P>,
    pattern: Arc<AtomicRawCell<LossPattern>>,
    inner_pattern: Box<LossPattern>,
    /// How many packets have been lost consecutively
    prev_loss: usize,
    rng: R,
    state: AtomicI32,
}

#[async_trait]
impl<P, R> Egress<P> for LossCellEgress<P, R>
where
    P: Packet + Send + Sync,
    R: Rng + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let packet = match self.egress.recv().await {
            Some(packet) => packet,
            None => return None,
        };
        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            0 => {
                return None;
            }
            1 => {
                return Some(packet);
            }
            _ => {}
        }
        if let Some(pattern) = self.pattern.swap_null() {
            self.inner_pattern = pattern;
            debug!(?self.inner_pattern, "Set inner pattern:");
        }
        let loss_rate = match self.inner_pattern.get(self.prev_loss) {
            Some(&loss_rate) => loss_rate,
            None => *self.inner_pattern.last().unwrap_or(&0.0),
        };
        let rand_num = self.rng.random_range(0.0..1.0);
        if rand_num < loss_rate {
            self.prev_loss += 1;
            None
        } else {
            self.prev_loss = 0;
            Some(packet)
        }
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }
}

// Loss pattern will repeat the last value until stop dropping packets.
// For example, the pattern [0.1, 0.2, 0.3] means [0.1, 0.2, 0.3, 0.3, 0.3, ...].
// Set the last value of the pattern to 0 to limit the maximum number of consecutive packet losses.
// If you want to drop packets i.i.d., just set the pattern to a single number, such as [0.1].
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Default, Clone)]
pub struct LossCellConfig {
    pub pattern: LossPattern,
}

impl LossCellConfig {
    pub fn new<T: Into<LossPattern>>(pattern: T) -> Self {
        Self {
            pattern: pattern.into(),
        }
    }
}

pub struct LossCellControlInterface {
    /// Stored as nanoseconds
    pattern: Arc<AtomicRawCell<LossPattern>>,
}

impl ControlInterface for LossCellControlInterface {
    type Config = LossCellConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Setting loss pattern to: {:?}", config.pattern);
        self.pattern.store(Box::new(config.pattern));
        Ok(())
    }
}

pub struct LossCell<P: Packet, R: Rng> {
    ingress: Arc<LossCellIngress<P>>,
    egress: LossCellEgress<P, R>,
    control_interface: Arc<LossCellControlInterface>,
}

impl<P, R> Cell<P> for LossCell<P, R>
where
    P: Packet + Send + Sync + 'static,
    R: Rng + Send + Sync + 'static,
{
    type IngressType = LossCellIngress<P>;
    type EgressType = LossCellEgress<P, R>;
    type ControlInterfaceType = LossCellControlInterface;

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
        Arc::clone(&self.control_interface)
    }
}

impl<P, R> LossCell<P, R>
where
    P: Packet,
    R: Rng,
{
    pub fn new<L: Into<LossPattern>>(pattern: L, rng: R) -> Result<LossCell<P, R>, Error> {
        let pattern = pattern.into();
        debug!(?pattern, "New LossCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let pattern = Arc::new(AtomicRawCell::new(Box::new(pattern)));
        Ok(LossCell {
            ingress: Arc::new(LossCellIngress { ingress: rx }),
            egress: LossCellEgress {
                egress: tx,
                pattern: Arc::clone(&pattern),
                inner_pattern: Box::default(),
                prev_loss: 0,
                rng,
                state: AtomicI32::new(0),
            },
            control_interface: Arc::new(LossCellControlInterface { pattern }),
        })
    }
}

type LossReplayCellIngress<P> = LossCellIngress<P>;

pub struct LossReplayCellEgress<P, R>
where
    P: Packet,
    R: Rng,
{
    egress: mpsc::UnboundedReceiver<P>,
    trace: Box<dyn LossTrace>,
    current_loss_pattern: LossPattern,
    next_change: Instant,
    config_rx: mpsc::UnboundedReceiver<LossReplayCellConfig>,
    change_timer: Timer,
    /// How many packets have been lost consecutively
    prev_loss: usize,
    rng: R,
    state: AtomicI32,
}

impl<P, R> LossReplayCellEgress<P, R>
where
    P: Packet + Send + Sync,
    R: Rng + Send + Sync,
{
    fn change_loss(&mut self, loss: LossPattern, change_time: Instant) {
        tracing::trace!(
            "Changing loss pattern to {:?} (should at {:?} ago)",
            loss,
            change_time.elapsed()
        );
        tracing::trace!(
            before = ?self.current_loss_pattern,
            after = ?loss,
            "Set inner loss pattern:"
        );
        self.current_loss_pattern = loss;
    }

    fn set_config(&mut self, config: LossReplayCellConfig) {
        tracing::debug!("Set inner trace config");
        self.trace = config.trace_config.into_model();
        let now = Instant::now();
        if self.next_change(now).is_none() {
            tracing::warn!("Setting null trace");
            self.next_change = now;
            // set state to 0 to indicate the trace goes to end and the cell will drop all packets
            self.change_state(0);
        }
    }

    fn next_change(&mut self, change_time: Instant) -> Option<()> {
        self.trace.next_loss().map(|(loss, duration)| {
            tracing::trace!(
                "Loss pattern changed to {:?}, next change after {:?}",
                loss,
                change_time + duration - Instant::now()
            );
            self.change_loss(loss, change_time);
            self.next_change = change_time + duration;
        })
    }
}

#[async_trait]
impl<P, R> Egress<P> for LossReplayCellEgress<P, R>
where
    P: Packet + Send + Sync,
    R: Rng + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let packet = loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.change_timer.sleep(self.next_change - Instant::now()) => {
                    if self.next_change(self.next_change).is_none() {
                        debug!("Trace goes to end");
                        self.egress.close();
                        return None;
                    }
                }
                packet = self.egress.recv() => if let Some(packet) = packet { break packet }
            }
        };
        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            0 => {
                return None;
            }
            1 => {
                return Some(packet);
            }
            _ => {}
        }
        let loss_rate = match self.current_loss_pattern.get(self.prev_loss) {
            Some(&loss_rate) => loss_rate,
            None => *self.current_loss_pattern.last().unwrap_or(&0.0),
        };
        let rand_num = self.rng.random_range(0.0..1.0);
        if rand_num < loss_rate {
            self.prev_loss += 1;
            None
        } else {
            self.prev_loss = 0;
            Some(packet)
        }
    }

    fn reset(&mut self) {
        self.prev_loss = 0;
        self.next_change = *CALIBRATED_START_INSTANT.get_or_init(Instant::now);
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LossReplayCellConfig {
    pub trace_config: Box<dyn LossTraceConfig>,
}

impl Clone for LossReplayCellConfig {
    fn clone(&self) -> Self {
        Self {
            trace_config: self.trace_config.clone(),
        }
    }
}

impl LossReplayCellConfig {
    pub fn new<T: Into<Box<dyn LossTraceConfig>>>(trace_config: T) -> Self {
        Self {
            trace_config: trace_config.into(),
        }
    }
}

pub struct LossReplayCellControlInterface {
    config_tx: mpsc::UnboundedSender<LossReplayCellConfig>,
}

impl ControlInterface for LossReplayCellControlInterface {
    type Config = LossReplayCellConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Setting loss replay config");
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct LossReplayCell<P: Packet, R: Rng> {
    ingress: Arc<LossReplayCellIngress<P>>,
    egress: LossReplayCellEgress<P, R>,
    control_interface: Arc<LossReplayCellControlInterface>,
}

impl<P, R> Cell<P> for LossReplayCell<P, R>
where
    P: Packet + Send + Sync + 'static,
    R: Rng + Send + Sync + 'static,
{
    type IngressType = LossReplayCellIngress<P>;
    type EgressType = LossReplayCellEgress<P, R>;
    type ControlInterfaceType = LossReplayCellControlInterface;

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

impl<P, R> LossReplayCell<P, R>
where
    P: Packet,
    R: Rng,
{
    pub fn new(trace: Box<dyn LossTrace>, rng: R) -> Result<LossReplayCell<P, R>, Error> {
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        let current_loss_pattern = vec![1.0];
        Ok(LossReplayCell {
            ingress: Arc::new(LossReplayCellIngress { ingress: rx }),
            egress: LossReplayCellEgress {
                egress: tx,
                trace,
                current_loss_pattern,
                next_change: Instant::now(),
                config_rx,
                change_timer: Timer::new()?,
                prev_loss: 0,
                rng,
                state: AtomicI32::new(0),
            },
            control_interface: Arc::new(LossReplayCellControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_json_snapshot;
    use itertools::iproduct;
    use netem_trace::model::{RepeatedLossPatternConfig, StaticLossConfig};
    use rand::{rngs::StdRng, SeedableRng};
    use std::time::Duration;
    use tracing::{span, Level};

    use crate::cells::StdPacket;

    use super::*;

    const LOSS_RATE_ACCURACY_TOLERANCE: f64 = 0.1;

    #[derive(Debug)]
    struct PacketStatistics {
        total: u32,
        lost: u32,
    }

    impl PacketStatistics {
        fn new() -> Self {
            Self { total: 0, lost: 0 }
        }

        fn get_lost_rate(&self) -> f64 {
            self.lost as f64 / self.total as f64
        }

        fn recv_loss_packet(&mut self) {
            self.total += 1;
            self.lost += 1;
        }

        fn recv_normal_packet(&mut self) {
            self.total += 1;
        }
    }

    fn get_loss_seq(pattern: Vec<f64>, rng_seed: u64) -> Result<Vec<bool>, Error> {
        let rt = tokio::runtime::Runtime::new()?;
        let _guard = rt.enter();
        let pattern_len = pattern.len();

        let mut cell: LossCell<StdPacket, StdRng> =
            LossCell::new(pattern, StdRng::seed_from_u64(rng_seed))?;
        let mut received_packets: Vec<bool> = Vec::with_capacity(100 * pattern_len);
        let ingress = cell.sender();
        let egress = cell.receiver();
        egress.reset();
        egress.change_state(2);

        for _ in 0..(100 * pattern_len) {
            let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });
            received_packets.push(received.is_some());
        }
        Ok(received_packets)
    }

    #[test_log::test]
    fn test_loss_cell() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_loss_cell").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with loss [0.1]");
        let cell = LossCell::new([0.1], StdRng::seed_from_u64(42))?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Testing loss for loss cell of loss [0.1]");
        let mut statistics = PacketStatistics::new();

        for _ in 0..100 {
            let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });

            match received {
                Some(content) => {
                    assert!(content.length() == 256);
                    statistics.recv_normal_packet();
                }
                None => statistics.recv_loss_packet(),
            }
        }
        let loss_rate = statistics.get_lost_rate();
        info!("Tested loss: {}", loss_rate);
        assert!((loss_rate - 0.1).abs() <= LOSS_RATE_ACCURACY_TOLERANCE);
        Ok(())
    }

    #[test_log::test]
    fn test_loss_cell_loss_list() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_loss_cell_loss_list").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        let loss_configs = vec![
            vec![0.1, 0.3, 0.8, 0.2],
            vec![0.1, 0.2, 0.8, 0.6, 0.5, 0.1],
            vec![0.8, 0.2, 0.8],
        ];

        let seeds: Vec<u64> = vec![42, 721, 2903, 100000];

        for (loss_config, seed) in iproduct!(loss_configs, seeds) {
            info!(
                "Testing loss cell with config {:?}, rng seed {}",
                loss_config.clone(),
                seed
            );
            let loss_seq = get_loss_seq(loss_config, seed)?;
            // Because tests are run with root privileges, insta review will not have sufficient privilege to update the snapshot file.
            // To update the snapshot, set the environment variable INSTA_UPDATE to always
            // so that insta will update the snapshot file during the test run (but without confirming).
            assert_json_snapshot!(loss_seq)
        }
        Ok(())
    }

    #[test_log::test]
    fn test_loss_cell_config_update() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_loss_cell_config_update").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with loss [1.0, 0.0]");
        let cell = LossCell::new([1.0, 0.0], StdRng::seed_from_u64(42))?;
        let config_changer = cell.control_interface();
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Sending a packet to transfer to second state");
        ingress.enqueue(StdPacket::from_raw_buffer(&[0; 256]))?;
        let received = rt.block_on(async { egress.dequeue().await });
        assert!(received.is_none());

        info!("Changing the config to [0.0, 1.0]");

        config_changer.set_config(LossCellConfig::new([0.0, 1.0]))?;

        // The packet should always be lost

        for _ in 0..100 {
            ingress.enqueue(StdPacket::from_raw_buffer(&[0; 256]))?;
            let received = rt.block_on(async { egress.dequeue().await });

            assert!(received.is_none());
        }

        Ok(())
    }

    #[test_log::test]
    fn test_loss_cell_config_update_length_change() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_loss_cell_config_update_fallback").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with loss [1.0, 1.0, 0.0]");
        let cell = LossCell::new([1.0, 1.0, 0.0], StdRng::seed_from_u64(42))?;
        let config_changer = cell.control_interface();
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Sending 2 packet to transfer to 3rd state");
        for _ in 0..2 {
            ingress.enqueue(StdPacket::from_raw_buffer(&[0; 256]))?;
            let received = rt.block_on(async { egress.dequeue().await });
            assert!(received.is_none());
        }

        info!("Changing config to [0.0, 1.0]");
        config_changer.set_config(LossCellConfig::new([0.0, 1.0]))?;

        // Now the loss rate should fall back to the last available, 1.0
        for _ in 0..100 {
            ingress.enqueue(StdPacket::from_raw_buffer(&[0; 256]))?;
            let received = rt.block_on(async { egress.dequeue().await });
            assert!(received.is_none());
        }

        info!("Changing config to [0.0, 0.0, 1.0]");
        config_changer.set_config(LossCellConfig::new([0.0, 0.0, 1.0]))?;

        // Now the lost packet is well over 3, thus the loss rate would still be 1
        for _ in 0..100 {
            ingress.enqueue(StdPacket::from_raw_buffer(&[0; 256]))?;
            let received = rt.block_on(async { egress.dequeue().await });
            assert!(received.is_none());
        }

        Ok(())
    }

    #[test_log::test]
    fn test_loss_replay_cell() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_loss_replay_cell").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        let pattern = vec![
            Box::new(
                StaticLossConfig::new()
                    .loss(vec![0.0])
                    .duration(Duration::from_secs(1)),
            ) as Box<dyn LossTraceConfig>,
            Box::new(
                StaticLossConfig::new()
                    .loss(vec![0.5])
                    .duration(Duration::from_secs(1)),
            ) as Box<dyn LossTraceConfig>,
        ];
        let loss_trace_config = Box::new(RepeatedLossPatternConfig::new().pattern(pattern).count(0))
            as Box<dyn LossTraceConfig>;
        let loss_trace = loss_trace_config.into_model();

        let cell: LossReplayCell<StdPacket, StdRng> =
            LossReplayCell::new(loss_trace, StdRng::seed_from_u64(42))?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        let start_time = tokio::time::Instant::now();

        for _ in 0..100 {
            let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });
            // Should drop all packets
            assert!(received.is_none());
        }
        egress.change_state(1);
        rt.block_on(async {
            tokio::time::sleep_until(start_time + Duration::from_millis(10)).await;
        });
        for _ in 0..100 {
            let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });
            // Should never loss packet
            assert!(received.is_some());
            assert!(received.unwrap().length() == 256)
        }
        egress.change_state(2);

        for (interval, calibrated_loss_rate) in [(1100, 0.5), (2100, 0.0)] {
            rt.block_on(async {
                tokio::time::sleep_until(start_time + Duration::from_millis(interval)).await;
            });
            let mut statistics = PacketStatistics::new();

            for _ in 0..100 {
                let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
                ingress.enqueue(test_packet)?;
                let received = rt.block_on(async { egress.dequeue().await });

                statistics.total += 1;
                match received {
                    Some(content) => assert!(content.length() == 256),
                    None => statistics.lost += 1,
                }
            }
            let loss_rate = statistics.get_lost_rate();
            info!(
                "Tested loss rate is {} and calibrated loss rate is {}",
                loss_rate, calibrated_loss_rate
            );
            assert!((loss_rate - calibrated_loss_rate).abs() <= LOSS_RATE_ACCURACY_TOLERANCE);
        }
        Ok(())
    }
}
