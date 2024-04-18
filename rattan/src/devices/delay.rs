use crate::devices::{Device, Packet};
use crate::error::Error;
use crate::metal::timer::Timer;
use async_trait::async_trait;
use netem_trace::Delay;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info};

use super::{ControlInterface, Egress, Ingress};

pub struct DelayDeviceIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for DelayDeviceIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for DelayDeviceIngress<P>
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

pub struct DelayDeviceEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<P>,
    delay: Delay,
    config_rx: mpsc::UnboundedReceiver<DelayDeviceConfig>,
    timer: Timer,
}

impl<P> DelayDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    fn set_config(&mut self, config: DelayDeviceConfig) {
        debug!(
            before = ?self.delay,
            after = ?config.delay,
            "Set inner delay:"
        );
        self.delay = config.delay;
    }
}

#[async_trait]
impl<P> Egress<P> for DelayDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let packet = match self.egress.recv().await {
            Some(packet) => packet,
            None => return None,
        };
        loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.timer.sleep(packet.get_timestamp() + self.delay - Instant::now()) => {
                    break;
                }
            }
        }
        Some(packet)
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Default, Clone)]
pub struct DelayDeviceConfig {
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub delay: Delay,
}

impl DelayDeviceConfig {
    pub fn new<T: Into<Delay>>(delay: T) -> Self {
        Self {
            delay: delay.into(),
        }
    }
}

pub struct DelayDeviceControlInterface {
    config_tx: mpsc::UnboundedSender<DelayDeviceConfig>,
}

impl ControlInterface for DelayDeviceControlInterface {
    type Config = DelayDeviceConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Setting delay to {:?}", config.delay);
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct DelayDevice<P: Packet> {
    ingress: Arc<DelayDeviceIngress<P>>,
    egress: DelayDeviceEgress<P>,
    control_interface: Arc<DelayDeviceControlInterface>,
}

impl<P> Device<P> for DelayDevice<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = DelayDeviceIngress<P>;
    type EgressType = DelayDeviceEgress<P>;
    type ControlInterfaceType = DelayDeviceControlInterface;

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

impl<P> DelayDevice<P>
where
    P: Packet,
{
    pub fn new<D: Into<Option<Delay>>>(delay: D) -> Result<DelayDevice<P>, Error> {
        let delay = delay.into().unwrap_or_default();
        debug!(?delay, "New DelayDevice");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(DelayDevice {
            ingress: Arc::new(DelayDeviceIngress { ingress: rx }),
            egress: DelayDeviceEgress {
                egress: tx,
                delay,
                config_rx,
                timer: Timer::new()?,
            },
            control_interface: Arc::new(DelayDeviceControlInterface { config_tx }),
        })
    }
}
