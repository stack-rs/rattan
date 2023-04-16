use crate::devices::{Device, Packet};
use crate::error::Error;
use async_trait::async_trait;
use std::fmt::Debug;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration, Instant};

use super::{ControlInterface, Egress, Ingress};

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
    /// Stored as nanoseconds
    delay: Arc<AtomicU64>,
}

#[async_trait]
impl<P> Egress<P> for DelayedDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let packet = self.egress.recv().await.unwrap();
        let delay = Duration::from_nanos(self.delay.load(Ordering::Relaxed));
        let queuing_delay = Instant::now() - packet.ingress_time;
        if queuing_delay < delay {
            sleep(delay - queuing_delay).await;
        }
        Some(packet.packet)
    }
}

pub struct DelayDeviceConfig {
    delay: Duration,
}

impl DelayDeviceConfig {
    pub fn new(delay: Duration) -> Self {
        Self { delay }
    }
}

#[derive(Debug)]
pub struct DelayDeviceControlInterface {
    /// Stored as nanoseconds
    delay: Arc<AtomicU64>,
}

impl ControlInterface for DelayDeviceControlInterface {
    type Config = DelayDeviceConfig;

    fn set_config(&mut self, config: Self::Config) -> Result<(), Error> {
        let delay = u64::try_from(config.delay.as_nanos()).map_err(|e| {
            Error::ConfigError(format!("Delay must be less than 2^64 nanoseconds: {}", e))
        })?;
        self.delay.store(delay, Ordering::Relaxed);
        Ok(())
    }
}

pub struct DelayDevice<P: Packet> {
    ingress: Arc<DelayDeviceIngress<P>>,
    egress: DelayedDeviceEgress<P>,
    control_interface: Arc<DelayDeviceControlInterface>,
}

impl<P> Device<P> for DelayDevice<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = DelayDeviceIngress<P>;
    type EgressType = DelayedDeviceEgress<P>;
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
        let delay = Arc::new(AtomicU64::new(0));
        DelayDevice {
            ingress: Arc::new(DelayDeviceIngress { ingress: rx }),
            egress: DelayedDeviceEgress {
                egress: tx,
                delay: Arc::clone(&delay),
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
