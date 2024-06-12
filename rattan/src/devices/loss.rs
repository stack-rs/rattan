use crate::devices::{Device, Packet};
use crate::error::Error;
use crate::utils::sync::AtomicRawCell;
use async_trait::async_trait;
use netem_trace::LossPattern;
use rand::Rng;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info};

use super::{ControlInterface, Egress, Ingress};

pub struct LossDeviceIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for LossDeviceIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for LossDeviceIngress<P>
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

pub struct LossDeviceEgress<P, R>
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
}

#[async_trait]
impl<P, R> Egress<P> for LossDeviceEgress<P, R>
where
    P: Packet + Send + Sync,
    R: Rng + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let packet = match self.egress.recv().await {
            Some(packet) => packet,
            None => return None,
        };
        if let Some(pattern) = self.pattern.swap_null() {
            self.inner_pattern = pattern;
            debug!(?self.inner_pattern, "Set inner pattern:");
        }
        let loss_rate = match self.inner_pattern.get(self.prev_loss) {
            Some(&loss_rate) => loss_rate,
            None => *self.inner_pattern.last().unwrap_or(&0.0),
        };
        let rand_num = self.rng.gen_range(0.0..1.0);
        if rand_num < loss_rate {
            self.prev_loss += 1;
            None
        } else {
            self.prev_loss = 0;
            Some(packet)
        }
    }
}

// Loss pattern will repeat the last value until stop dropping packets.
// For example, the pattern [0.1, 0.2, 0.3] means [0.1, 0.2, 0.3, 0.3, 0.3, ...].
// Set the last value of the pattern to 0 to limit the maximum number of consecutive packet losses.
// If you want to drop packets i.i.d., just set the pattern to a single number, such as [0.1].
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Default, Clone)]
pub struct LossDeviceConfig {
    pub pattern: LossPattern,
}

impl LossDeviceConfig {
    pub fn new<T: Into<LossPattern>>(pattern: T) -> Self {
        Self {
            pattern: pattern.into(),
        }
    }
}

pub struct LossDeviceControlInterface {
    /// Stored as nanoseconds
    pattern: Arc<AtomicRawCell<LossPattern>>,
}

impl ControlInterface for LossDeviceControlInterface {
    type Config = LossDeviceConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Setting loss pattern to: {:?}", config.pattern);
        self.pattern.store(Box::new(config.pattern));
        Ok(())
    }
}

pub struct LossDevice<P: Packet, R: Rng> {
    ingress: Arc<LossDeviceIngress<P>>,
    egress: LossDeviceEgress<P, R>,
    control_interface: Arc<LossDeviceControlInterface>,
}

impl<P, R> Device<P> for LossDevice<P, R>
where
    P: Packet + Send + Sync + 'static,
    R: Rng + Send + Sync + 'static,
{
    type IngressType = LossDeviceIngress<P>;
    type EgressType = LossDeviceEgress<P, R>;
    type ControlInterfaceType = LossDeviceControlInterface;

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

impl<P, R> LossDevice<P, R>
where
    P: Packet,
    R: Rng,
{
    pub fn new<L: Into<LossPattern>>(pattern: L, rng: R) -> Result<LossDevice<P, R>, Error> {
        let pattern = pattern.into();
        debug!(?pattern, "New LossDevice");
        let (rx, tx) = mpsc::unbounded_channel();
        let pattern = Arc::new(AtomicRawCell::new(Box::new(pattern)));
        Ok(LossDevice {
            ingress: Arc::new(LossDeviceIngress { ingress: rx }),
            egress: LossDeviceEgress {
                egress: tx,
                pattern: Arc::clone(&pattern),
                inner_pattern: Box::default(),
                prev_loss: 0,
                rng,
            },
            control_interface: Arc::new(LossDeviceControlInterface { pattern }),
        })
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_json_snapshot;
    use itertools::iproduct;
    use rand::{rngs::StdRng, SeedableRng};
    use tracing::{span, Level};

    use crate::devices::StdPacket;

    use super::*;

    const LOSS_RATE_ACCURACY_TOLERANCE: f64 = 0.1;

    #[derive(Debug)]
    struct PacketStatistics {
        total: i32,
        lost: i32,
    }

    impl PacketStatistics {
        fn new() -> Self {
            Self { total: 0, lost: 0 }
        }

        fn get_lost_rate(&self) -> f64 {
            self.lost as f64 / self.total as f64
        }
    }

    fn get_loss_seq(pattern: Vec<f64>, rng_seed: u64) -> Result<Vec<bool>, Error> {
        let rt = tokio::runtime::Runtime::new()?;
        let _guard = rt.enter();
        let pattern_len = pattern.len();

        let mut device: LossDevice<StdPacket, StdRng> =
            LossDevice::new(pattern, StdRng::seed_from_u64(rng_seed))?;
        let mut received_packets: Vec<bool> = Vec::with_capacity(100 * pattern_len);
        let ingress = device.sender();
        let egress = device.receiver();

        for _ in 0..(100 * pattern_len) {
            let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });
            received_packets.push(received.is_some());
        }
        Ok(received_packets)
    }

    #[test_log::test]
    fn test_loss_device() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_loss_device").entered();
        let rt = tokio::runtime::Runtime::new()?;
        let _guard = rt.enter();

        info!("Creating device with loss [0.1]");
        let device_config = LossDeviceConfig::new([0.1]);
        let builder = device_config.into_factory::<StdPacket>();
        let device = builder(rt.handle())?;
        let ingress = device.sender();
        let mut egress = device.into_receiver();

        info!("Testing loss for loss device of loss [0.1]");
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
        info!("Tested loss: {}", loss_rate);
        assert!((loss_rate - 0.1).abs() <= LOSS_RATE_ACCURACY_TOLERANCE);
        Ok(())
    }

    #[test_log::test]
    fn test_loss_device_loss_list() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_loss_device_loss_list").entered();
        let rt = tokio::runtime::Runtime::new()?;
        let _guard = rt.enter();

        let loss_configs = vec![
            vec![0.1, 0.3, 0.8, 0.2],
            vec![0.1, 0.2, 0.8, 0.6, 0.5, 0.1],
            vec![0.8, 0.2, 0.8],
        ];

        let seeds: Vec<u64> = vec![42, 721, 2903, 100000];

        for (loss_config, seed) in iproduct!(loss_configs, seeds) {
            info!(
                "Testing loss device with config {:?}, rng seed {}",
                loss_config.clone(),
                seed
            );
            let loss_seq = get_loss_seq(loss_config, seed)?;
            /* Because tests are run with root privileges, insta review will not have sufficient privilege to update the snapshot file. To update the snapshot, set the environment variable INSTA_UPDATE to always so that insta will update the snapshot file during the test run (but without confirming). */
            assert_json_snapshot!(loss_seq)
        }
        Ok(())
    }

    #[test_log::test]
    fn test_loss_device_config_update() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_loss_device_config_update").entered();
        let rt = tokio::runtime::Runtime::new()?;
        let _guard = rt.enter();

        info!("Creating device with loss [1.0, 0.0]");
        let device_config = LossDeviceConfig::new([1.0, 0.0]);
        let builder = device_config.into_factory::<StdPacket>();
        let device = builder(rt.handle())?;
        let config_changer = device.control_interface();
        let ingress = device.sender();
        let mut egress = device.into_receiver();

        info!("Sending a packet to transfer to second state");
        ingress.enqueue(StdPacket::from_raw_buffer(&[0; 256]))?;
        let received = rt.block_on(async { egress.dequeue().await });
        assert!(received.is_none());

        info!("Changing the config to [0.0, 1.0]");

        config_changer.set_config(LossDeviceConfig::new([0.0, 1.0]))?;

        // The packet should always be lost

        for _ in 0..100 {
            ingress.enqueue(StdPacket::from_raw_buffer(&[0; 256]))?;
            let received = rt.block_on(async { egress.dequeue().await });

            assert!(received.is_none());
        }

        Ok(())
    }

    #[test_log::test]
    fn test_loss_device_config_update_length_change() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_loss_device_config_update_fallback").entered();
        let rt = tokio::runtime::Runtime::new()?;
        let _guard = rt.enter();

        info!("Creating device with loss [1.0, 1.0, 0.0]");
        let device_config = LossDeviceConfig::new([1.0, 1.0, 0.0]);
        let builder = device_config.into_factory::<StdPacket>();
        let device = builder(rt.handle())?;
        let config_changer = device.control_interface();
        let ingress = device.sender();
        let mut egress = device.into_receiver();

        info!("Sending 2 packet to transfer to 3rd state");
        for _ in 0..2 {
            ingress.enqueue(StdPacket::from_raw_buffer(&[0; 256]))?;
            let received = rt.block_on(async { egress.dequeue().await });
            assert!(received.is_none());
        }

        info!("Changing config to [0.0, 1.0]");
        config_changer.set_config(LossDeviceConfig::new([0.0, 1.0]))?;

        // Now the loss rate should fall back to the last available, 1.0
        for _ in 0..100 {
            ingress.enqueue(StdPacket::from_raw_buffer(&[0; 256]))?;
            let received = rt.block_on(async { egress.dequeue().await });
            assert!(received.is_none());
        }

        info!("Changing config to [0.0, 0.0, 1.0]");
        config_changer.set_config(LossDeviceConfig::new([0.0, 0.0, 1.0]))?;

        // Now the lost packet is well over 3, thus the loss rate would still be 1
        for _ in 0..100 {
            ingress.enqueue(StdPacket::from_raw_buffer(&[0; 256]))?;
            let received = rt.block_on(async { egress.dequeue().await });
            assert!(received.is_none());
        }

        Ok(())
    }
}
