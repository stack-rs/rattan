use crate::devices::{Device, Packet};
use crate::error::Error;
use crate::utils::sync::AtomicRawCell;
use async_trait::async_trait;
#[cfg(feature = "serde")]
use duration_str::deserialize_duration;
use netem_trace::Delay;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep, Instant};

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
        // XXX(minhuw): handle possible error here
        self.ingress
            .send(DelayPacket {
                ingress_time: Instant::now(),
                packet,
            })
            .unwrap();
        Ok(())
    }
}

pub struct DelayDeviceEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<DelayPacket<P>>,
    delay: Arc<AtomicRawCell<Delay>>,
    inner_delay: Box<Delay>,
}

#[async_trait]
impl<P> Egress<P> for DelayDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let packet = self.egress.recv().await.unwrap();
        if let Some(delay) = self.delay.swap_null() {
            self.inner_delay = delay;
        }
        let queuing_delay = Instant::now() - packet.ingress_time;
        if queuing_delay < *self.inner_delay {
            sleep(*self.inner_delay - queuing_delay).await;
        }
        Some(packet.packet)
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DelayDeviceConfig {
    #[cfg_attr(feature = "serde", serde(deserialize_with = "deserialize_duration"))]
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
    delay: Arc<AtomicRawCell<Delay>>,
}

impl ControlInterface for DelayDeviceControlInterface {
    type Config = DelayDeviceConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        println!("Setting delay to {:?}", config.delay);
        self.delay.store(Box::new(config.delay));
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
        let (rx, tx) = mpsc::unbounded_channel();
        let delay = Arc::new(AtomicRawCell::new(Box::default()));
        DelayDevice {
            ingress: Arc::new(DelayDeviceIngress { ingress: rx }),
            egress: DelayDeviceEgress {
                egress: tx,
                delay: Arc::clone(&delay),
                inner_delay: Box::default(),
            },
            control_interface: Arc::new(DelayDeviceControlInterface { delay }),
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
