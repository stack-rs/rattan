use crate::devices::bandwidth::queue::PacketQueue;
use crate::devices::{Device, Packet};
use crate::error::Error;
use crate::metal::timer::Timer;
use async_trait::async_trait;
use netem_trace::{model::BwTraceConfig, Bandwidth, BwTrace, Delay};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tracing::{debug, info, trace, warn};

use super::{ControlInterface, Egress, Ingress};

pub mod queue;

pub const MAX_BANDWIDTH: Bandwidth = Bandwidth::from_bps(u64::MAX);
pub const LARGE_DURATION: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

// Length should be the network layer length, not the link layer length
// Requires the bandwidth to be less than 2^64 bps
fn transfer_time(length: usize, bandwidth: Bandwidth, bw_type: BwType) -> Delay {
    let bits = (length + bw_type.extra_length()) * 8;
    let capacity = bandwidth.as_bps() as u64;
    if capacity == 0 {
        LARGE_DURATION
    } else {
        let seconds = bits as f64 / capacity as f64;
        Delay::from_secs_f64(seconds)
    }
}

// Bandwidth calculation type, deciding the extra length of the packet
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum BwType {
    LinkLayer, // + 38 = 8 (Preamble + SFD) + 14 (Ethernet header) + 4 (CRC) + 12 (Interframe gap)
    #[default]
    NetworkLayer, // + 0
}

impl BwType {
    pub fn extra_length(&self) -> usize {
        match self {
            BwType::LinkLayer => 38,
            BwType::NetworkLayer => 0,
        }
    }
}

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
        self.ingress
            .send(packet)
            .map_err(|_| Error::ChannelError("Data channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct BwDeviceEgress<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    egress: mpsc::UnboundedReceiver<P>,
    bw_type: BwType,
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
            let now = Instant::now();
            debug!(
                "Previous next_available distance: {:?}",
                self.next_available - now
            );
            self.next_available = if bandwidth == Bandwidth::from_bps(0) {
                now + LARGE_DURATION
            } else {
                now + (self.next_available - now)
                    .mul_f64(self.bandwidth.as_bps() as f64 / bandwidth.as_bps() as f64)
            };
            debug!(
                before = ?self.bandwidth,
                after = ?bandwidth,
                "Set inner bandwidth:"
            );
            debug!(
                "Now next_available distance: {:?}",
                self.next_available - now
            );
            self.bandwidth = bandwidth;
        }
        if let Some(queue_config) = config.queue_config {
            debug!(?queue_config, "Set inner queue config:");
            self.packet_queue.configure(queue_config);
        }
        if let Some(bw_type) = config.bw_type {
            debug!(?bw_type, "Set inner bw_type:");
            self.bw_type = bw_type;
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
        let transfer_time = transfer_time(packet.l3_length(), self.bandwidth, self.bw_type);
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
    pub bandwidth: Option<Bandwidth>,
    pub queue_config: Option<Q::Config>,
    pub bw_type: Option<BwType>,
}

impl<P, Q> Clone for BwDeviceConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
    Q::Config: Clone,
{
    fn clone(&self) -> Self {
        Self {
            bandwidth: self.bandwidth,
            queue_config: self.queue_config.clone(),
            bw_type: self.bw_type,
        }
    }
}

impl<P, Q> BwDeviceConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new<T: Into<Option<Bandwidth>>, U: Into<Option<Q::Config>>, V: Into<Option<BwType>>>(
        bandwidth: T,
        queue_config: U,
        bw_type: V,
    ) -> Self {
        Self {
            bandwidth: bandwidth.into(),
            queue_config: queue_config.into(),
            bw_type: bw_type.into(),
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
        if config.bandwidth.is_none() && config.queue_config.is_none() && config.bw_type.is_none() {
            // This ensures that incorrect HTTP requests will return errors.
            return Err(Error::ConfigError(
                "At least one of bandwidth, queue_config and bw_type should be set".to_string(),
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
        if let Some(bw_type) = config.bw_type {
            info!("Setting bw_type to: {:?}", bw_type);
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
    pub fn new<B: Into<Option<Bandwidth>>, BT: Into<Option<BwType>>>(
        bandwidth: B,
        packet_queue: Q,
        bw_type: BT,
    ) -> Result<BwDevice<P, Q>, Error> {
        debug!("New BwDevice");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(BwDevice {
            ingress: Arc::new(BwDeviceIngress { ingress: rx }),
            egress: BwDeviceEgress {
                egress: tx,
                bw_type: bw_type.into().unwrap_or_default(),
                bandwidth: bandwidth.into().unwrap_or(MAX_BANDWIDTH),
                packet_queue,
                next_available: Instant::now(),
                config_rx,
                timer: Timer::new()?,
            },
            control_interface: Arc::new(BwDeviceControlInterface { config_tx }),
        })
    }
}

type BwReplayDeviceIngress<P> = BwDeviceIngress<P>;

pub struct BwReplayDeviceEgress<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    egress: mpsc::UnboundedReceiver<P>,
    bw_type: BwType,
    trace: Box<dyn BwTrace>,
    packet_queue: Q,
    current_bandwidth: Bandwidth,
    next_available: Instant,
    next_change: Instant,
    config_rx: mpsc::UnboundedReceiver<BwReplayDeviceConfig<P, Q>>,
    send_timer: Timer,
    change_timer: Timer,
}

impl<P, Q> BwReplayDeviceEgress<P, Q>
where
    P: Packet + Send + Sync,
    Q: PacketQueue<P>,
{
    fn change_bandwidth(&mut self, bandwidth: Bandwidth, change_time: Instant) {
        trace!(
            "Changing bandwidth to {:?} (should at {:?} ago)",
            bandwidth,
            change_time.elapsed()
        );
        trace!(
            "Previous next_available distance: {:?}",
            self.next_available - change_time
        );
        self.next_available = if bandwidth == Bandwidth::from_bps(0) {
            Instant::now() + LARGE_DURATION
        } else {
            change_time
                + (self.next_available - change_time)
                    .mul_f64(self.current_bandwidth.as_bps() as f64 / bandwidth.as_bps() as f64)
        };
        trace!(
            before = ?self.current_bandwidth,
            after = ?bandwidth,
            "Set inner bandwidth:"
        );
        trace!(
            "Now next_available distance: {:?}",
            self.next_available - change_time
        );
        self.current_bandwidth = bandwidth;
    }

    fn set_config(&mut self, config: BwReplayDeviceConfig<P, Q>) {
        if let Some(trace_config) = config.trace_config {
            debug!("Set inner trace config");
            self.trace = trace_config.into_model();
            let now = Instant::now();
            match self.trace.next_bw() {
                Some((bandwidth, duration)) => {
                    self.change_bandwidth(bandwidth, now);
                    self.next_change = now + duration;
                    trace!(
                        "Bandwidth changed to {:?}, next change after {:?}",
                        bandwidth,
                        self.next_change - Instant::now()
                    );
                }
                None => {
                    // handle null trace outside this function
                    warn!("Setting null trace");
                    self.change_bandwidth(Bandwidth::from_bps(0), now);
                    self.next_change = now;
                }
            }
        }
        if let Some(queue_config) = config.queue_config {
            debug!(?queue_config, "Set inner queue config:");
            self.packet_queue.configure(queue_config);
        }
        if let Some(bw_type) = config.bw_type {
            debug!(?bw_type, "Set inner bw_type:");
            self.bw_type = bw_type;
        }
    }

    // Return the next change time or **None** if the trace goes to end
    fn next_change(&mut self, change_time: Instant) -> Option<Instant> {
        let next_bw = self.trace.next_bw();
        next_bw.map(|(bandwidth, duration)| {
            self.change_bandwidth(bandwidth, change_time);
            trace!(
                "Bandwidth changed to {:?}, next change after {:?}",
                bandwidth,
                change_time + duration - Instant::now()
            );
            change_time + duration
        })
    }
}

#[async_trait]
impl<P, Q> Egress<P> for BwReplayDeviceEgress<P, Q>
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
                _ = self.change_timer.sleep(self.next_change - Instant::now()) => {
                    match self.next_change(self.next_change) {
                        Some(next_change) => {
                            self.next_change = next_change;
                        }
                        None => {
                            debug!("Trace goes to end");
                            self.egress.close();
                            return None;
                        }
                    }
                }
                _ = self.send_timer.sleep(self.next_available - Instant::now()) => {
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
                _ = self.change_timer.sleep(self.next_change - Instant::now()) => {
                    match self.next_change(self.next_change) {
                        Some(next_change) => {
                            self.next_change = next_change;
                        }
                        None => {
                            debug!("Trace goes to end");
                            self.egress.close();
                            return None;
                        }
                    }
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
        let transfer_time = transfer_time(packet.l3_length(), self.current_bandwidth, self.bw_type);
        if packet.get_timestamp() >= self.next_available {
            // the packet arrives after next_available
            self.next_available = packet.get_timestamp() + transfer_time;
        } else {
            // the packet arrives before next_available and now >= self.next_available
            self.next_available += transfer_time;
        }
        Some(packet)
    }

    // This must be called before any dequeue
    fn reset(&mut self) {
        self.next_available = Instant::now();
        self.next_change = Instant::now();
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
pub struct BwReplayDeviceConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub trace_config: Option<Box<dyn BwTraceConfig>>,
    pub queue_config: Option<Q::Config>,
    pub bw_type: Option<BwType>,
}

impl<P, Q> Clone for BwReplayDeviceConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
    Q::Config: Clone,
{
    fn clone(&self) -> Self {
        Self {
            trace_config: self.trace_config.clone(),
            queue_config: self.queue_config.clone(),
            bw_type: self.bw_type,
        }
    }
}

impl<P, Q> BwReplayDeviceConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new<
        T: Into<Option<Box<dyn BwTraceConfig>>>,
        U: Into<Option<Q::Config>>,
        V: Into<Option<BwType>>,
    >(
        trace_config: T,
        queue_config: U,
        bw_type: V,
    ) -> Self {
        Self {
            trace_config: trace_config.into(),
            queue_config: queue_config.into(),
            bw_type: bw_type.into(),
        }
    }
}

pub struct BwReplayDeviceControlInterface<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    config_tx: mpsc::UnboundedSender<BwReplayDeviceConfig<P, Q>>,
}

impl<P, Q> ControlInterface for BwReplayDeviceControlInterface<P, Q>
where
    P: Packet,
    Q: PacketQueue<P> + 'static,
{
    type Config = BwReplayDeviceConfig<P, Q>;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        if config.trace_config.is_none()
            && config.queue_config.is_none()
            && config.bw_type.is_none()
        {
            // This ensures that incorrect HTTP requests will return errors.
            return Err(Error::ConfigError(
                "At least one of bandwidth, queue_config and bw_type should be set".to_string(),
            ));
        }
        if let Some(_trace_config) = config.trace_config.as_ref() {
            #[cfg(feature = "serde")]
            info!(
                "Setting trace config to: {}",
                serde_json::to_string(_trace_config).unwrap()
            );
            #[cfg(not(feature = "serde"))]
            info!("Setting trace config");
        }
        if let Some(queue_config) = config.queue_config.as_ref() {
            info!("Setting queue config to: {:?}", queue_config);
        }
        if let Some(bw_type) = config.bw_type {
            info!("Setting bw_type to: {:?}", bw_type);
        }
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct BwReplayDevice<P: Packet, Q: PacketQueue<P>> {
    ingress: Arc<BwReplayDeviceIngress<P>>,
    egress: BwReplayDeviceEgress<P, Q>,
    control_interface: Arc<BwReplayDeviceControlInterface<P, Q>>,
}

impl<P, Q> Device<P> for BwReplayDevice<P, Q>
where
    P: Packet + Send + Sync + 'static,
    Q: PacketQueue<P> + 'static,
{
    type IngressType = BwReplayDeviceIngress<P>;
    type EgressType = BwReplayDeviceEgress<P, Q>;
    type ControlInterfaceType = BwReplayDeviceControlInterface<P, Q>;

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

impl<P, Q> BwReplayDevice<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new<BT: Into<Option<BwType>>>(
        trace: Box<dyn BwTrace>,
        packet_queue: Q,
        bw_type: BT,
    ) -> Result<BwReplayDevice<P, Q>, Error> {
        debug!("New BwReplayDevice");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(BwReplayDevice {
            ingress: Arc::new(BwReplayDeviceIngress { ingress: rx }),
            egress: BwReplayDeviceEgress {
                egress: tx,
                bw_type: bw_type.into().unwrap_or_default(),
                trace,
                packet_queue,
                current_bandwidth: Bandwidth::from_bps(0),
                next_available: Instant::now(),
                next_change: Instant::now(),
                config_rx,
                send_timer: Timer::new()?,
                change_timer: Timer::new()?,
            },
            control_interface: Arc::new(BwReplayDeviceControlInterface { config_tx }),
        })
    }
}
