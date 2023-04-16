use crate::devices::{Device, Packet};
use crate::error::Error;
use async_trait::async_trait;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration, Instant};

use super::{Egress, Ingress};

#[derive(Debug)]
struct DelayedPacket<P>
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
    ingress: mpsc::UnboundedSender<DelayedPacket<P>>,
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
            .send(DelayedPacket {
                ingress_time: Instant::now(),
                packet,
            })
            .unwrap();
        Ok(())
    }
}

pub struct DelayedDeviceEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<DelayedPacket<P>>,
    delay: Duration,
}

#[async_trait]
impl<P> Egress<P> for DelayedDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let packet = self.egress.recv().await.unwrap();
        let queuing_delay = Instant::now() - packet.ingress_time;
        if queuing_delay < self.delay {
            sleep(self.delay - queuing_delay).await;
        }
        Some(packet.packet)
    }
}

pub struct DelayDeviceConfig {
    delay: Duration,
}

pub struct DelayDevice<P: Packet> {
    ingress: Arc<DelayDeviceIngress<P>>,
    egress: DelayedDeviceEgress<P>,
    config: DelayDeviceConfig,
}

impl<P> Device<P> for DelayDevice<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = DelayDeviceIngress<P>;
    type EgressType = DelayedDeviceEgress<P>;
    type Config = DelayDeviceConfig;

    fn sender(&self) -> Arc<Self::IngressType> {
        self.ingress.clone()
    }

    fn receiver(&mut self) -> &mut Self::EgressType {
        &mut self.egress
    }

    fn into_receiver(self) -> Self::EgressType {
        self.egress
    }

    fn set_config(&mut self, config: Self::Config) -> Result<(), Error> {
        self.config = config;
        self.egress.delay = self.config.delay;
        Ok(())
    }
}

impl<P> DelayDevice<P>
where
    P: Packet,
{
    pub fn new() -> DelayDevice<P> {
        let (rx, tx) = mpsc::unbounded_channel();
        DelayDevice {
            ingress: Arc::new(DelayDeviceIngress { ingress: rx }),
            egress: DelayedDeviceEgress {
                egress: tx,
                delay: Duration::ZERO,
            },
            config: DelayDeviceConfig {
                delay: Duration::ZERO,
            },
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
