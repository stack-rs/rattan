use crate::devices::bandwidth::queue::PacketQueue;
use crate::devices::{Device, Packet};
use crate::error::Error;
use crate::metal::timer::Timer;
use async_trait::async_trait;
use netem_trace::{Bandwidth, Delay};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info};

use super::{ControlInterface, Egress, Ingress};

pub mod queue;

pub const MAX_BANDWIDTH: Bandwidth = Bandwidth::from_bps(u64::MAX);

pub struct BwDeviceIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
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
    fn enqueue(&self, mut packet: P) -> Result<(), Error> {
        packet.set_timestamp(Instant::now());
        // XXX(minhuw): handle possible error here
        self.ingress.send(packet).unwrap();
        Ok(())
    }
}

// Requires the bandwidth to be less than 2^64 bps
fn transfer_time(length: usize, bandwidth: Bandwidth) -> Delay {
    let bits = length * 8;
    let capacity = bandwidth.as_bps() as u64;
    let seconds = bits as f64 / capacity as f64;
    Delay::from_secs_f64(seconds)
}

pub struct BwDeviceEgress<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    egress: mpsc::UnboundedReceiver<P>,
    bandwidth: Bandwidth,
    packet_queue: Q,
    next_available: Instant,
    config_rx: mpsc::UnboundedReceiver<BwDeviceConfig<P, Q>>,
    timer: Timer,
}

impl<P, Q> BwDeviceEgress<P, Q>
where
    P: Packet + Send + Sync,
    Q: PacketQueue<P>,
{
    fn set_config(&mut self, config: BwDeviceConfig<P, Q>) {
        if let Some(bandwidth) = config.bandwidth {
            debug!(
                "Previous next_available distance: {:?}",
                self.next_available - Instant::now()
            );
            self.next_available = self.next_available
                + (self.next_available - Instant::now())
                    .mul_f64(self.bandwidth.as_bps() as f64 / bandwidth.as_bps() as f64);
            debug!(
                before = ?self.bandwidth,
                after = ?bandwidth,
                "Set inner bandwidth:"
            );
            debug!(
                "Now next_available distance: {:?}",
                self.next_available - Instant::now()
            );
            self.bandwidth = bandwidth;
        }
        if let Some(queue_config) = config.queue_config {
            debug!(?queue_config, "Set inner queue config:");
            self.packet_queue.configure(queue_config);
        }
    }
}

#[async_trait]
impl<P, Q> Egress<P> for BwDeviceEgress<P, Q>
where
    P: Packet + Send + Sync,
    Q: PacketQueue<P>,
{
    async fn dequeue(&mut self) -> Option<P> {
        // wait until next_available
        loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.timer.sleep(self.next_available - Instant::now()) => {
                    break;
                }
            }
        }
        // process the packets received during sleep time
        while let Ok(new_packet) = self.egress.try_recv() {
            self.packet_queue.enqueue(new_packet);
        }

        let mut packet = self.packet_queue.dequeue();
        while packet.is_none() {
            // the queue is empty, wait for the next packet
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                recv_packet = self.egress.recv() => {
                    match recv_packet {
                        Some(new_packet) => {
                            self.packet_queue.enqueue(new_packet);
                            packet = self.packet_queue.dequeue();
                        }
                        None => {
                            // channel closed
                            return None;
                        }
                    }
                }
            }
        }

        // send the packet
        let packet = packet.unwrap();
        let transfer_time = transfer_time(packet.length(), self.bandwidth);
        if packet.get_timestamp() >= self.next_available {
            // the packet arrives after next_available
            self.next_available = packet.get_timestamp() + transfer_time;
        } else {
            // the packet arrives before next_available and now >= self.next_available
            self.next_available += transfer_time;
        }
        Some(packet)
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug)]
pub struct BwDeviceConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    bandwidth: Option<Bandwidth>,
    queue_config: Option<Q::Config>,
}

impl<P, Q> BwDeviceConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new<T: Into<Option<Bandwidth>>, U: Into<Option<Q::Config>>>(
        bandwidth: T,
        queue_config: U,
    ) -> Self {
        Self {
            bandwidth: bandwidth.into(),
            queue_config: queue_config.into(),
        }
    }
}

pub struct BwDeviceControlInterface<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    config_tx: mpsc::UnboundedSender<BwDeviceConfig<P, Q>>,
}

impl<P, Q> ControlInterface for BwDeviceControlInterface<P, Q>
where
    P: Packet,
    Q: PacketQueue<P> + 'static,
{
    type Config = BwDeviceConfig<P, Q>;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        if config.bandwidth.is_none() && config.queue_config.is_none() {
            // This ensures that incorrect HTTP requests will return errors.
            return Err(Error::ConfigError(
                "At least one of bandwidth and queue_config should be set".to_string(),
            ));
        }
        if let Some(bandwidth) = config.bandwidth {
            if bandwidth > MAX_BANDWIDTH {
                return Err(Error::ConfigError(
                    "Bandwidth should be less than 2^64 bps".to_string(),
                ));
            }
            info!("Setting bandwidth to: {:?}", bandwidth);
        }
        if let Some(queue_config) = config.queue_config.as_ref() {
            info!("Setting queue config to: {:?}", queue_config);
        }
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct BwDevice<P: Packet, Q: PacketQueue<P>> {
    ingress: Arc<BwDeviceIngress<P>>,
    egress: BwDeviceEgress<P, Q>,
    control_interface: Arc<BwDeviceControlInterface<P, Q>>,
}

impl<P, Q> Device<P> for BwDevice<P, Q>
where
    P: Packet + Send + Sync + 'static,
    Q: PacketQueue<P> + 'static,
{
    type IngressType = BwDeviceIngress<P>;
    type EgressType = BwDeviceEgress<P, Q>;
    type ControlInterfaceType = BwDeviceControlInterface<P, Q>;

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

impl<P, Q> BwDevice<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new(bandwidth: Bandwidth, packet_queue: Q) -> BwDevice<P, Q> {
        debug!("New BwDevice");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        BwDevice {
            ingress: Arc::new(BwDeviceIngress { ingress: rx }),
            egress: BwDeviceEgress {
                egress: tx,
                bandwidth,
                packet_queue,
                next_available: Instant::now(),
                config_rx,
                timer: Timer::new().unwrap(),
            },
            control_interface: Arc::new(BwDeviceControlInterface { config_tx }),
        }
    }
}
