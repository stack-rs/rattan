use crate::devices::{Device, Packet};
use crate::error::Error;
use crate::utils::sync::AtomicRawCell;
use async_trait::async_trait;
use netem_trace::{Bandwidth, Delay};
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_timerfd::Delay as DelayTimer;

use super::{ControlInterface, Egress, Ingress};

const MAX_BANDWIDTH: Bandwidth = Bandwidth::from_bps(u64::MAX);

#[derive(Debug)]
struct BwPacket<P>
where
    P: Packet,
{
    ingress_time: Instant,
    length: usize,
    packet: P,
}

pub struct BwDeviceIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<BwPacket<P>>,
}

impl<P> Clone for BwDeviceIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for BwDeviceIngress<P>
where
    P: Packet + Send,
{
    fn enqueue(&self, packet: P) -> Result<(), Error> {
        // XXX(minhuw): handle possible error here
        self.ingress
            .send(BwPacket {
                ingress_time: Instant::now(),
                length: packet.length(),
                packet,
            })
            .unwrap();
        Ok(())
    }
}

// Requires the bandwidth to be less than 2^64 bps
fn transfer_time(length: usize, bandwidth: Bandwidth) -> Delay {
    let bits = length * 8;
    let capacity = bandwidth.as_bps() as u64;
    let seconds = bits as f64 / capacity as f64;
    // println!("{} {} {}", bits, capacity, seconds);
    Delay::from_secs_f64(seconds)
}

pub struct BwDeviceEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<BwPacket<P>>,
    bandwidth: Arc<AtomicRawCell<Bandwidth>>,
    inner_bandwidth: Box<Bandwidth>,
    next_available: Instant,
    timer: Arc<DelayTimer>,
}

#[async_trait]
impl<P> Egress<P> for BwDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let packet = self.egress.recv().await.unwrap();
        if let Some(delay) = self.bandwidth.swap_null() {
            self.inner_bandwidth = delay;
        }
        let now = Instant::now();
        if packet.ingress_time >= self.next_available {
            // no need to wait, since the packet arrives after next_available
            let transfer_time = transfer_time(packet.length, *self.inner_bandwidth);
            self.next_available = packet.ingress_time + transfer_time;
        } else if now >= self.next_available {
            // the current time is already after next_available
            let transfer_time = transfer_time(packet.length, *self.inner_bandwidth);
            self.next_available += transfer_time;
        } else {
            // wait until next_available
            let timer = Arc::get_mut(&mut self.timer).unwrap();
            timer.reset(self.next_available.into_std());
            timer.await.unwrap();
            let transfer_time = transfer_time(packet.length, *self.inner_bandwidth);
            self.next_available += transfer_time;
        }
        Some(packet.packet)
    }
}

pub struct BwDeviceConfig {
    bandwidth: Bandwidth,
}

impl BwDeviceConfig {
    pub fn new<T: Into<Bandwidth>>(bandwidth: T) -> Self {
        Self {
            bandwidth: bandwidth.into(),
        }
    }
}

pub struct BwDeviceControlInterface {
    bandwidth: Arc<AtomicRawCell<Bandwidth>>,
}

impl ControlInterface for BwDeviceControlInterface {
    type Config = BwDeviceConfig;

    fn set_config(&mut self, config: Self::Config) -> Result<(), Error> {
        if config.bandwidth > MAX_BANDWIDTH {
            return Err(Error::ConfigError(
                "Bandwidth should be less than 2^64 bps".to_string(),
            ));
        }
        self.bandwidth.store(Box::new(config.bandwidth));
        Ok(())
    }
}

pub struct BwDevice<P: Packet> {
    ingress: Arc<BwDeviceIngress<P>>,
    egress: BwDeviceEgress<P>,
    control_interface: Arc<BwDeviceControlInterface>,
}

impl<P> Device<P> for BwDevice<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = BwDeviceIngress<P>;
    type EgressType = BwDeviceEgress<P>;
    type ControlInterfaceType = BwDeviceControlInterface;

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

impl<P> BwDevice<P>
where
    P: Packet,
{
    pub fn new() -> BwDevice<P> {
        let (rx, tx) = mpsc::unbounded_channel();
        let bandwidth = Arc::new(AtomicRawCell::new(Box::new(Bandwidth::from_bps(u64::MAX))));
        let timer = DelayTimer::new(std::time::Instant::now()).unwrap();
        BwDevice {
            ingress: Arc::new(BwDeviceIngress { ingress: rx }),
            egress: BwDeviceEgress {
                egress: tx,
                bandwidth: Arc::clone(&bandwidth),
                inner_bandwidth: Box::new(Bandwidth::from_bps(u64::MAX)),
                next_available: Instant::now(),
                timer: Arc::new(timer),
            },
            control_interface: Arc::new(BwDeviceControlInterface { bandwidth }),
        }
    }
}

impl<P> Default for BwDevice<P>
where
    P: Packet,
{
    fn default() -> Self {
        Self::new()
    }
}
