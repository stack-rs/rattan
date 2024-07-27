use crate::devices::{Device, Packet};
use crate::error::Error;
use crate::metal::timer::Timer;
use crate::utils::sync::AtomicRawCell;
use async_trait::async_trait;
use netem_trace::model::DuplicateTraceConfig;
use netem_trace::{DuplicatePattern, DuplicateTrace};
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

pub struct DuplicateDeviceIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for DuplicateDeviceIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for DuplicateDeviceIngress<P>
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

pub struct DuplicateDeviceEgress<P, R>
where
    P: Packet,
    R: Rng,
{
    egress: mpsc::UnboundedReceiver<P>,
    pattern: Arc<AtomicRawCell<DuplicatePattern>>,
    inner_pattern: Box<DuplicatePattern>,
    /// How many *different* packets have been duplicated consecutively
    prev_duplicated: usize,
    duplicated_buffer: Option<P>,
    rng: R,
    state: AtomicI32,
}

#[async_trait]
impl<P, R> Egress<P> for DuplicateDeviceEgress<P, R>
where
    P: Packet + Send + Sync,
    R: Rng + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // Simply return previously duplicated packet, if any
        let buffer = self.duplicated_buffer.take();
        if let Some(packet) = buffer {
            return Some(packet);
        }

        debug!("Waiting for a result from ingress");
        let mut packet = match self.egress.recv().await {
            Some(packet) => packet,
            None => return None,
        };

        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            0 => return None,
            1 => return Some(packet),
            _ => {}
        }

        if let Some(pattern) = self.pattern.swap_null() {
            self.inner_pattern = pattern;
            debug!(?self.inner_pattern, "Set inner pattern:");
        }

        let duplicate_rate = match self.inner_pattern.get(self.prev_duplicated) {
            Some(&duplicate_rate) => duplicate_rate,
            None => *self.inner_pattern.last().unwrap_or(&0.0),
        };

        let rand_num = self.rng.gen_range(0.0..1.0);
        debug!("Duplicate rate {}, rand num {}", duplicate_rate, rand_num);
        if rand_num < duplicate_rate {
            self.prev_duplicated += 1;
            let mut duplicated_packet = P::from_raw_buffer(packet.as_raw_buffer());
            duplicated_packet.set_timestamp(packet.get_timestamp());
            self.duplicated_buffer = Some(duplicated_packet);
        } else {
            self.prev_duplicated = 0;
        }
        Some(packet)
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }
}

// Duplicate pattern will repeat the last value until stop duplicating packets.
// For example, the pattern [0.1, 0.2, 0.3] means [0.1, 0.2, 0.3, 0.3, 0.3, ...].
// Set the last value of the pattern to 0 to limit the maxium number of consecutive packet being duplicated.
// If you want to duplicate packetes i.i.d., just set the pattern to a single number, such as [0.2].
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Default, Clone)]
pub struct DuplicateDeviceConfig {
    pub pattern: DuplicatePattern,
}

impl DuplicateDeviceConfig {
    pub fn new<T: Into<DuplicatePattern>>(pattern: T) -> Self {
        Self {
            pattern: pattern.into(),
        }
    }
}

pub struct DuplicateDeviceControlInterface {
    pattern: Arc<AtomicRawCell<DuplicatePattern>>,
}

impl ControlInterface for DuplicateDeviceControlInterface {
    type Config = DuplicateDeviceConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Settings duplicate pattern to: {:?}", config.pattern);
        self.pattern.store(Box::new(config.pattern));
        Ok(())
    }
}

pub struct DuplicateDevice<P, R>
where
    P: Packet,
    R: Rng,
{
    ingress: Arc<DuplicateDeviceIngress<P>>,
    egress: DuplicateDeviceEgress<P, R>,
    control_interface: Arc<DuplicateDeviceControlInterface>,
}

impl<P, R> Device<P> for DuplicateDevice<P, R>
where
    P: Packet + Send + Sync + 'static,
    R: Rng + Send + Sync + 'static,
{
    type IngressType = DuplicateDeviceIngress<P>;
    type EgressType = DuplicateDeviceEgress<P, R>;
    type ControlInterfaceType = DuplicateDeviceControlInterface;

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

impl<P, R> DuplicateDevice<P, R>
where
    P: Packet,
    R: Rng,
{
    pub fn new<T: Into<DuplicatePattern>>(
        pattern: T,
        rng: R,
    ) -> Result<DuplicateDevice<P, R>, Error> {
        let pattern = pattern.into();
        debug!(?pattern, "New DuplicateDevice");
        let (tx, rx) = mpsc::unbounded_channel();
        let pattern = Arc::new(AtomicRawCell::new(Box::new(pattern)));
        Ok(DuplicateDevice {
            ingress: Arc::new(DuplicateDeviceIngress { ingress: tx }),
            egress: DuplicateDeviceEgress {
                egress: rx,
                pattern: Arc::clone(&pattern),
                inner_pattern: Box::default(),
                prev_duplicated: 0,
                duplicated_buffer: None,
                rng,
                state: AtomicI32::new(0),
            },
            control_interface: Arc::new(DuplicateDeviceControlInterface { pattern }),
        })
    }
}

type DuplicateReplayDeviceIngress<P> = DuplicateDeviceIngress<P>;

pub struct DuplicateReplayDeviceEgress<P, R>
where
    P: Packet,
    R: Rng,
{
    egress: mpsc::UnboundedReceiver<P>,
    trace: Box<dyn DuplicateTrace>,
    current_duplicate_pattern: DuplicatePattern,
    next_change: Instant,
    config_rx: mpsc::UnboundedReceiver<DuplicateReplayDeviceConfig>,
    change_timer: Timer,
    /// How many packets have been duplicated consecutively
    prev_duplicated: usize,
    duplicated_buffer: Option<P>,
    rng: R,
    state: AtomicI32,
}

impl<P, R> DuplicateReplayDeviceEgress<P, R>
where
    P: Packet + Send + Sync,
    R: Rng + Send + Sync,
{
    fn change_duplicate(&mut self, duplicate: DuplicatePattern, change_time: Instant) {
        tracing::trace!(
            "Changing duplicate pattern to {:?} (should at {:?} ago)",
            duplicate,
            change_time.elapsed()
        );
        tracing::trace!(
            before = ?self.current_duplicate_pattern,
            after = ?duplicate,
            "Set innter duplicate pattern:"
        );
        self.current_duplicate_pattern = duplicate;
    }

    fn set_config(&mut self, config: DuplicateReplayDeviceConfig) {
        tracing::debug!("Set inner trace config");
        self.trace = config.trace_config.into_model();
        let now = Instant::now();
        if self.next_change(now).is_none() {
            tracing::warn!("Settings null trace");
            self.next_change = now;
            // set state to 0 to indicate the trace goes to end and the device will drop all packets
            self.change_state(0);
        }
    }

    fn next_change(&mut self, change_time: Instant) -> Option<()> {
        self.trace.next_duplicate().map(|(duplicate, duration)| {
            tracing::trace!(
                "Duplicate pattern changed to {:?}, next change after {:?}",
                duplicate,
                change_time + duration - Instant::now()
            );
            self.change_duplicate(duplicate, change_time);
            self.next_change = change_time + duration;
        })
    }
}

#[async_trait]
impl<P, R> Egress<P> for DuplicateReplayDeviceEgress<P, R>
where
    P: Packet + Send + Sync,
    R: Rng + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let buffer = self.duplicated_buffer.take();
        if let Some(packet) = buffer {
            return Some(packet);
        }

        let mut packet = loop {
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
        let duplicate_rate = match self.current_duplicate_pattern.get(self.prev_duplicated) {
            Some(&duplicate_rate) => duplicate_rate,
            None => *self.current_duplicate_pattern.last().unwrap_or(&0.0),
        };
        let rand_num = self.rng.gen_range(0.0..1.0);
        if rand_num < duplicate_rate {
            self.prev_duplicated += 1;
            let mut duplicated_packet = P::from_raw_buffer(packet.as_raw_buffer());
            duplicated_packet.set_timestamp(packet.get_timestamp());
            self.duplicated_buffer = Some(duplicated_packet);
        } else {
            self.prev_duplicated = 0;
        }
        Some(packet)
    }

    fn reset(&mut self) {
        self.prev_duplicated = 0;
        self.next_change = Instant::now();
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DuplicateReplayDeviceConfig {
    pub trace_config: Box<dyn DuplicateTraceConfig>,
}

impl Clone for DuplicateReplayDeviceConfig {
    fn clone(&self) -> Self {
        Self {
            trace_config: self.trace_config.clone(),
        }
    }
}

impl DuplicateReplayDeviceConfig {
    pub fn new<T: Into<Box<dyn DuplicateTraceConfig>>>(trace_config: T) -> Self {
        Self {
            trace_config: trace_config.into(),
        }
    }
}

pub struct DuplicateReplayDeviceControlInterface {
    config_tx: mpsc::UnboundedSender<DuplicateReplayDeviceConfig>,
}

impl ControlInterface for DuplicateReplayDeviceControlInterface {
    type Config = DuplicateReplayDeviceConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Settings duplicate replay config");
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct DuplicateReplayDevice<P, R>
where
    P: Packet,
    R: Rng,
{
    ingress: Arc<DuplicateReplayDeviceIngress<P>>,
    egress: DuplicateReplayDeviceEgress<P, R>,
    control_interface: Arc<DuplicateReplayDeviceControlInterface>,
}

impl<P, R> Device<P> for DuplicateReplayDevice<P, R>
where
    P: Packet + Send + Sync + 'static,
    R: Rng + Send + Sync + 'static,
{
    type IngressType = DuplicateReplayDeviceIngress<P>;
    type EgressType = DuplicateReplayDeviceEgress<P, R>;
    type ControlInterfaceType = DuplicateReplayDeviceControlInterface;

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

impl<P, R> DuplicateReplayDevice<P, R>
where
    P: Packet,
    R: Rng,
{
    pub fn new(
        trace: Box<dyn DuplicateTrace>,
        rng: R,
    ) -> Result<DuplicateReplayDevice<P, R>, Error> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        let current_duplicate_pattern = vec![1.0];
        Ok(DuplicateReplayDevice {
            ingress: Arc::new(DuplicateReplayDeviceIngress { ingress: tx }),
            egress: DuplicateReplayDeviceEgress {
                egress: rx,
                trace,
                current_duplicate_pattern,
                next_change: Instant::now(),
                config_rx,
                change_timer: Timer::new()?,
                prev_duplicated: 0,
                duplicated_buffer: None,
                rng,
                state: AtomicI32::new(0),
            },
            control_interface: Arc::new(DuplicateReplayDeviceControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_json_snapshot;
    use itertools::iproduct;
    use netem_trace::model::{RepeatedDuplicatePatternConfig, StaticDuplicateConfig};
    use rand::{rngs::StdRng, SeedableRng};
    use std::time::Duration;
    use tracing::{span, Level};

    use crate::devices::StdPacket;

    use super::*;

    const DUPLICATE_RATE_ACCURACY_TOLERANCE: f64 = 0.05;

    #[derive(Debug)]
    struct PacketStatistics {
        total: u32,
        duplicated: u32,
    }

    impl PacketStatistics {
        fn new() -> Self {
            Self {
                total: 0,
                duplicated: 0,
            }
        }

        fn get_duplicate_rate(&self) -> f64 {
            self.duplicated as f64 / self.total as f64
        }

        fn recv_duplicated_packet(&mut self) {
            self.duplicated += 1;
        }

        fn recv_normal_packet(&mut self) {
            self.total += 1;
        }
    }

    fn get_duplicate_seq(pattern: DuplicatePattern, rng_seed: u64) -> Result<Vec<bool>, Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();
        let pattern_len = pattern.len();

        let mut device: DuplicateDevice<StdPacket, StdRng> =
            DuplicateDevice::new(pattern, StdRng::seed_from_u64(rng_seed))?;
        let mut received_packets: Vec<bool> = vec![];
        let ingress = device.sender();
        let egress = device.receiver();
        egress.reset();
        egress.change_state(2);

        for idx in 0..(100 * pattern_len) {
            let test_packet = StdPacket::from_raw_buffer(&idx.to_ne_bytes());
            ingress.enqueue(test_packet)?;
        }
        let mut prev_received: Option<StdPacket> = None;
        rt.block_on(async {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(5)) => break,
                    Some(mut received) = egress.dequeue() => {
                        // Every time we received a packet, we can determine whether the previous packet is dupliated.
                        match prev_received {
                            None => {
                                // prev_received is cleared, meaning 1) this packet is the first packet, or 2) the previous packet
                                // is a duplication of the packet before the previous one, under either circumstances,
                                // there is no need to record the outcome.
                                prev_received = Some(received);
                            },
                            Some(mut prev_packet) => {
                                // The previous one is not a duplication. If current is same as it, then the previous one is duplicated;
                                // if not, the previous one is not duplicated.
                                if prev_packet.as_raw_buffer() == received.as_raw_buffer() {
                                    received_packets.push(true);
                                    prev_received = None;
                                } else {
                                    received_packets.push(false);
                                    prev_received = Some(received);
                                }
                            }
                        }
                    }
                }
            }
        });
        Ok(received_packets)
    }

    #[test_log::test]
    fn test_duplicate_device() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_duplicate_device").entered();
        info!("Running duplicate device test");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating device with duplicate [0.1]");
        let mut device = DuplicateDevice::new([0.1], StdRng::seed_from_u64(42))?;
        let ingress = device.sender();
        let egress = device.receiver();
        egress.reset();
        egress.change_state(2);

        info!("Testing duplicate for duplicate device of duplicate [0.1]");

        for idx in 0..500u32 {
            let test_packet = StdPacket::from_raw_buffer(&idx.to_ne_bytes());
            ingress.enqueue(test_packet)?;
        }

        let mut statistics = PacketStatistics::new();
        let mut prev_received: Option<StdPacket> = None;
        // The recv() method used in Egress will sleep when there is no packet in the device.
        // Since this device *theoretically* has no delay, use 1 sec as a timeout so that the process
        // won't block on this last dequeue().
        rt.block_on(async {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(5)) => break,
                    Some(mut received) = egress.dequeue() => {
                        match prev_received {
                            None => {
                                statistics.recv_normal_packet();
                                prev_received = Some(received);
                            },
                            Some(mut prev_packet) => {
                                if prev_packet.as_raw_buffer() == received.as_raw_buffer() {
                                    statistics.recv_duplicated_packet();
                                    prev_received = None;
                                } else {
                                    statistics.recv_normal_packet();
                                    prev_received = Some(received);
                                }
                            }
                        }
                    }
                }
            }
        });

        let duplicate_rate = statistics.get_duplicate_rate();
        info!("Tested dupliate rate: {}", duplicate_rate);
        assert!((duplicate_rate - 0.1).abs() <= DUPLICATE_RATE_ACCURACY_TOLERANCE);
        Ok(())
    }

    #[test_log::test]
    fn test_duplicate_device_duplicate_list() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_duplicate_device_duplicate_list").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        let duplicate_configs = vec![
            vec![0.1, 0.3, 0.8, 0.2],
            vec![0.1, 0.2, 0.8, 0.6, 0.5, 0.1],
            vec![0.8, 0.2, 0.8],
        ];

        let seeds: Vec<u64> = vec![42, 721, 2903, 100000];

        for (duplicate_config, seed) in iproduct!(duplicate_configs, seeds) {
            info!(
                "Testing duplicate device with config {:?}, rng seed {}",
                duplicate_config, seed
            );
            let duplicate_seq = get_duplicate_seq(duplicate_config, seed)?;
            // Because tests are run with root privileges, insta review will not have sufficient privilege to update the snapshot file.
            // To update the snapshot, set the environment variable INSTA_UPDATE to always
            // so that insta will update the snapshot file during the test run (but without confirming).
            assert_json_snapshot!(duplicate_seq);
        }

        Ok(())
    }

    #[test_log::test]
    fn test_duplicate_device_config_update() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_duplicate_device_config_update").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating device with loss [1.0, 0.0]");
        let mut device: DuplicateDevice<StdPacket, StdRng> =
            DuplicateDevice::new([1.0, 0.0], StdRng::seed_from_u64(42))?;
        let config_changer = device.control_interface();
        let ingress = device.sender();
        let egress = device.receiver();
        egress.reset();
        egress.change_state(2);

        let mut duplicate_seq: Vec<bool> = vec![];
        let mut prev_received: Option<StdPacket> = None;

        for idx in 0..3u32 {
            ingress.enqueue(StdPacket::from_raw_buffer(&idx.to_ne_bytes()))?;
        }

        rt.block_on(async {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(5)) => break,
                    Some(mut received) = egress.dequeue() => {
                        match prev_received {
                            None => {
                                prev_received = Some(received);
                            },
                            Some(mut prev_packet) => {
                                if prev_packet.as_raw_buffer() == received.as_raw_buffer() {
                                    duplicate_seq.push(true);
                                    prev_received = None;
                                } else {
                                    duplicate_seq.push(false);
                                    prev_received = Some(received);
                                }
                            }
                        }
                    }
                }
            }
        });

        assert_eq!(duplicate_seq, vec![true, false, true]);

        info!("Changing the config to [0.0, 1.0]");
        config_changer.set_config(DuplicateDeviceConfig::new([0.0, 1.0]))?;

        // The packet should always be duplicated
        // All packets should be duplicated
        for idx in 4..7u32 {
            ingress.enqueue(StdPacket::from_raw_buffer(&idx.to_ne_bytes()))?;
        }

        let mut duplicate_seq: Vec<bool> = vec![];
        let mut prev_received: Option<StdPacket> = None;

        rt.block_on(async {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(5)) => break,
                    Some(mut received) = egress.dequeue() => {
                        match prev_received {
                            None => {
                                prev_received = Some(received);
                            },
                            Some(mut prev_packet) => {
                                if prev_packet.as_raw_buffer() == received.as_raw_buffer() {
                                    duplicate_seq.push(true);
                                    prev_received = None;
                                } else {
                                    duplicate_seq.push(false);
                                    prev_received = Some(received);
                                }
                            }
                        }
                    }
                }
            }
        });

        assert_eq!(duplicate_seq, vec![true, true, true]);

        Ok(())
    }

    #[test_log::test]
    fn test_duplicate_device_update_length_change() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_duplicate_device_config_update_fallback").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating device with duplicate rate [1.0, 1.0, 0.0]");
        let mut device: DuplicateDevice<StdPacket, StdRng> =
            DuplicateDevice::new([1.0, 1.0, 0.0], StdRng::seed_from_u64(42))?;
        let config_changer = device.control_interface();
        let ingress = device.sender();
        let egress = device.receiver();
        egress.reset();
        egress.change_state(2);

        info!("Sending 2 packet to transfer to 3rd state");
        for idx in 0..2u32 {
            ingress.enqueue(StdPacket::from_raw_buffer(&idx.to_ne_bytes()))?;
        }

        let mut duplicate_seq: Vec<bool> = vec![];
        let mut prev_received: Option<StdPacket> = None;

        rt.block_on(async {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(5)) => break,
                    Some(mut received) = egress.dequeue() => {
                        match prev_received {
                            None => {
                                prev_received = Some(received);
                            },
                            Some(mut prev_packet) => {
                                if prev_packet.as_raw_buffer() == received.as_raw_buffer() {
                                    duplicate_seq.push(true);
                                    prev_received = None;
                                } else {
                                    duplicate_seq.push(false);
                                    prev_received = Some(received);
                                }
                            }
                        }
                    }
                }
            }
        });

        assert_eq!(duplicate_seq, vec![true, true]);

        info!("Changing config to [0.0, 1.0]");
        config_changer.set_config(DuplicateDeviceConfig::new([0.0, 1.0]))?;

        for idx in 3..6u32 {
            ingress.enqueue(StdPacket::from_raw_buffer(&idx.to_ne_bytes()))?;
        }

        let mut duplicate_seq: Vec<bool> = vec![];
        let mut prev_received: Option<StdPacket> = None;

        rt.block_on(async {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(5)) => break,
                    Some(mut received) = egress.dequeue() => {
                        match prev_received {
                            None => {
                                prev_received = Some(received);
                            },
                            Some(mut prev_packet) => {
                                if prev_packet.as_raw_buffer() == received.as_raw_buffer() {
                                    duplicate_seq.push(true);
                                    prev_received = None;
                                } else {
                                    duplicate_seq.push(false);
                                    prev_received = Some(received);
                                }
                            }
                        }
                    }
                }
            }
        });

        assert_eq!(duplicate_seq, vec![true, true, true]);

        info!("Changing config to [0.0, 0.0, 1.0]");
        config_changer.set_config(DuplicateDeviceConfig::new([0.0, 0.0, 1.0]))?;

        for idx in 7..9u32 {
            ingress.enqueue(StdPacket::from_raw_buffer(&idx.to_ne_bytes()))?;
        }

        let mut duplicate_seq: Vec<bool> = vec![];
        let mut prev_received: Option<StdPacket> = None;

        rt.block_on(async {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(5)) => break,
                    Some(mut received) = egress.dequeue() => {
                        match prev_received {
                            None => {
                                prev_received = Some(received);
                            },
                            Some(mut prev_packet) => {
                                if prev_packet.as_raw_buffer() == received.as_raw_buffer() {
                                    duplicate_seq.push(true);
                                    prev_received = None;
                                } else {
                                    duplicate_seq.push(false);
                                    prev_received = Some(received);
                                }
                            }
                        }
                    }
                }
            }
        });

        assert_eq!(duplicate_seq, vec![true, true]);

        Ok(())
    }

    #[test_log::test]
    fn test_duplicate_replay_device() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_duplicate_replay_device").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        let pattern = vec![
            Box::new(
                StaticDuplicateConfig::new()
                    .duplicate(vec![0.0])
                    .duration(Duration::from_secs(1)),
            ) as Box<dyn DuplicateTraceConfig>,
            Box::new(
                StaticDuplicateConfig::new()
                    .duplicate(vec![0.5])
                    .duration(Duration::from_secs(1)),
            ) as Box<dyn DuplicateTraceConfig>,
        ];
        let duplicate_trace_config = Box::new(
            RepeatedDuplicatePatternConfig::new()
                .pattern(pattern)
                .count(0),
        ) as Box<dyn DuplicateTraceConfig>;
        let duplicate_trace = duplicate_trace_config.into_model();

        let mut device: DuplicateReplayDevice<StdPacket, StdRng> =
            DuplicateReplayDevice::new(duplicate_trace, StdRng::seed_from_u64(42))?;
        let ingress = device.sender();
        let egress = device.receiver();
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
        for (interval, calibrated_duplicate_rate) in [(1100, 0.5), (2100, 0.0)] {
            rt.block_on(async {
                tokio::time::sleep_until(start_time + Duration::from_millis(interval)).await;
            });

            for idx in 0..500u32 {
                let test_packet = StdPacket::from_raw_buffer(&idx.to_ne_bytes());
                ingress.enqueue(test_packet)?;
            }

            let mut statistics = PacketStatistics::new();
            let mut prev_received: Option<StdPacket> = None;

            rt.block_on(async {
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(tokio::time::Duration::from_millis(5)) => break,
                        Some(mut received) = egress.dequeue() => {
                            match prev_received {
                                None => {
                                    statistics.recv_normal_packet();
                                    prev_received = Some(received);
                                },
                                Some(mut prev_packet) => {
                                    if prev_packet.as_raw_buffer() == received.as_raw_buffer() {
                                        statistics.recv_duplicated_packet();
                                        prev_received = None;
                                    } else {
                                        statistics.recv_normal_packet();
                                        prev_received = Some(received);
                                    }
                                }
                            }
                        }
                    }
                }
            });

            let duplicate_rate = statistics.get_duplicate_rate();
            info!(
                "Tested duplicate rate is {} and calibrated duplicated rate is {}",
                duplicate_rate, calibrated_duplicate_rate
            );
            assert!(
                (duplicate_rate - calibrated_duplicate_rate).abs()
                    <= DUPLICATE_RATE_ACCURACY_TOLERANCE
            );
        }
        Ok(())
    }
}
