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

#[derive(Debug)]
struct DelayPacket<P>
where
    P: Packet,
{
    ingress_time: Instant,
    packet: P,
}

pub struct DelayDeviceIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<DelayPacket<P>>,
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
    fn enqueue(&self, packet: P) -> Result<(), Error> {
        self.ingress
            .send(DelayPacket {
                ingress_time: Instant::now(),
                packet,
            })
            .map_err(|_| Error::ChannelError("Data channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct DelayDeviceEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<DelayPacket<P>>,
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
        let packet = self.egress.recv().await.unwrap();
        loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.timer.sleep(packet.ingress_time + self.delay - Instant::now()) => {
                    break;
                }
            }
        }
        Some(packet.packet)
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DelayDeviceConfig {
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    delay: Delay,
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
    pub fn new() -> DelayDevice<P> {
        debug!("New DelayDevice");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        DelayDevice {
            ingress: Arc::new(DelayDeviceIngress { ingress: rx }),
            egress: DelayDeviceEgress {
                egress: tx,
                delay: Delay::default(),
                config_rx,
                timer: Timer::new().unwrap(),
            },
            control_interface: Arc::new(DelayDeviceControlInterface { config_tx }),
        }
    }
}

impl<P> Default for DelayDevice<P>
where
    P: Packet,
{
    fn default() -> Self {
        Self::new()
    }
}
