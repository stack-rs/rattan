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

#[derive(Debug)]
struct LossPacket<P>
where
    P: Packet,
{
    packet: P,
}

pub struct LossDeviceIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<LossPacket<P>>,
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
    fn enqueue(&self, packet: P) -> Result<(), Error> {
        self.ingress
            .send(LossPacket { packet })
            .map_err(|_| Error::ChannelError("Data channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct LossDeviceEgress<P, R>
where
    P: Packet,
    R: Rng,
{
    egress: mpsc::UnboundedReceiver<LossPacket<P>>,
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
        let packet = self.egress.recv().await.unwrap();
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
            Some(packet.packet)
        }
    }
}

// Loss pattern will repeat the last value until stop dropping packets.
// For example, the pattern [0.1, 0.2, 0.3] means [0.1, 0.2, 0.3, 0.3, 0.3, ...].
// Set the last value of the pattern to 0 to limit the maximum number of consecutive packet losses.
// If you want to drop packets i.i.d., just set the pattern to a single number, such as [0.1].
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug)]
pub struct LossDeviceConfig {
    pattern: LossPattern,
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
        info!("Set loss pattern to: {:?}", config.pattern);
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
    pub fn new(rng: R) -> LossDevice<P, R> {
        debug!("New LossDevice");
        let (rx, tx) = mpsc::unbounded_channel();
        let pattern = Arc::new(AtomicRawCell::new(Box::default()));
        LossDevice {
            ingress: Arc::new(LossDeviceIngress { ingress: rx }),
            egress: LossDeviceEgress {
                egress: tx,
                pattern: Arc::clone(&pattern),
                inner_pattern: Box::default(),
                prev_loss: 0,
                rng,
            },
            control_interface: Arc::new(LossDeviceControlInterface { pattern }),
        }
    }
}
