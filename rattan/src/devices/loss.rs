use crate::devices::{Device, Packet};
use crate::error::Error;
use crate::utils::sync::AtomicRawCell;
use async_trait::async_trait;
use netem_trace::LossPattern;
use rand::Rng;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::mpsc;

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
        // XXX(minhuw): handle possible error here
        self.ingress.send(LossPacket { packet }).unwrap();
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
        }
        // out-of-range loss rate is treated as 0
        if self.inner_pattern.len() <= self.prev_loss {
            self.prev_loss = 0;
            Some(packet.packet)
        } else {
            let loss_rate = self.inner_pattern[self.prev_loss];
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
}

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

    fn set_config(&mut self, config: Self::Config) -> Result<(), Error> {
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
