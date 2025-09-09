use super::TRACE_START_INSTANT;
use crate::cells::bandwidth::queue::PacketQueue;
use crate::cells::{Cell, Packet};
use crate::error::Error;
use crate::metal::timer::Timer;
use async_trait::async_trait;
use netem_trace::{model::BwTraceConfig, Bandwidth, BwTrace, Delay};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tracing::{debug, info, trace};

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

pub struct BwCellIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for BwCellIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for BwCellIngress<P>
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

pub struct BwCellEgress<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    egress: mpsc::UnboundedReceiver<P>,
    bw_type: BwType,
    bandwidth: Bandwidth,
    packet_queue: Q,
    next_available: Instant,
    config_rx: mpsc::UnboundedReceiver<BwCellConfig<P, Q>>,
    timer: Timer,
    state: AtomicI32,
    notify_rx: Option<tokio::sync::broadcast::Receiver<crate::control::RattanNotify>>,
    started: bool,
}

impl<P, Q> BwCellEgress<P, Q>
where
    P: Packet + Send + Sync,
    Q: PacketQueue<P>,
{
    fn set_config(&mut self, config: BwCellConfig<P, Q>) {
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
impl<P, Q> Egress<P> for BwCellEgress<P, Q>
where
    P: Packet + Send + Sync,
    Q: PacketQueue<P>,
{
    async fn dequeue(&mut self) -> Option<P> {
        // Wait for Start notify if not started yet
        crate::wait_until_started!(self, Start);

        // wait until next_available
        loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                recv_packet = self.egress.recv() => {
                    match recv_packet {
                        Some(new_packet) => {
                            match self.state.load(std::sync::atomic::Ordering::Acquire) {
                                0 => {
                                    return None;
                                }
                                1 => {
                                    return Some(new_packet);
                                }
                                _ => {
                                    self.packet_queue.enqueue(new_packet);
                                }
                            }
                        }
                        None => {
                            // channel closed
                            return None;
                        }
                    }
                }
                _ = self.timer.sleep(self.next_available - Instant::now()) => {
                    break;
                }
            }
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
                            match self.state.load(std::sync::atomic::Ordering::Acquire) {
                                0 => {
                                    return None;
                                }
                                1 => {
                                    return Some(new_packet);
                                }
                                _ => {
                                    self.packet_queue.enqueue(new_packet);
                                    packet = self.packet_queue.dequeue();
                                }
                            }
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

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }

    fn set_notify_receiver(
        &mut self,
        notify_rx: tokio::sync::broadcast::Receiver<crate::control::RattanNotify>,
    ) {
        self.notify_rx = Some(notify_rx);
    }
}

#[cfg_attr(
    feature = "serde",
    serde_with::skip_serializing_none,
    derive(Deserialize, Serialize)
)]
#[derive(Debug)]
pub struct BwCellConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    #[cfg_attr(feature = "serde", serde(with = "human_bandwidth::serde"))]
    pub bandwidth: Option<Bandwidth>,
    pub queue_config: Option<Q::Config>,
    pub bw_type: Option<BwType>,
}

impl<P, Q> Clone for BwCellConfig<P, Q>
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

impl<P, Q> BwCellConfig<P, Q>
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

pub struct BwCellControlInterface<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    config_tx: mpsc::UnboundedSender<BwCellConfig<P, Q>>,
}

impl<P, Q> ControlInterface for BwCellControlInterface<P, Q>
where
    P: Packet,
    Q: PacketQueue<P> + 'static,
{
    type Config = BwCellConfig<P, Q>;

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

pub struct BwCell<P: Packet, Q: PacketQueue<P>> {
    ingress: Arc<BwCellIngress<P>>,
    egress: BwCellEgress<P, Q>,
    control_interface: Arc<BwCellControlInterface<P, Q>>,
}

impl<P, Q> Cell<P> for BwCell<P, Q>
where
    P: Packet + Send + Sync + 'static,
    Q: PacketQueue<P> + 'static,
{
    type IngressType = BwCellIngress<P>;
    type EgressType = BwCellEgress<P, Q>;
    type ControlInterfaceType = BwCellControlInterface<P, Q>;

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

impl<P, Q> BwCell<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new<B: Into<Option<Bandwidth>>, BT: Into<Option<BwType>>>(
        bandwidth: B,
        packet_queue: Q,
        bw_type: BT,
    ) -> Result<BwCell<P, Q>, Error> {
        debug!("New BwCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(BwCell {
            ingress: Arc::new(BwCellIngress { ingress: rx }),
            egress: BwCellEgress {
                egress: tx,
                bw_type: bw_type.into().unwrap_or_default(),
                bandwidth: bandwidth.into().unwrap_or(MAX_BANDWIDTH),
                packet_queue,
                next_available: Instant::now(),
                config_rx,
                timer: Timer::new()?,
                state: AtomicI32::new(0),
                notify_rx: None,
                started: false,
            },
            control_interface: Arc::new(BwCellControlInterface { config_tx }),
        })
    }
}

type BwReplayCellIngress<P> = BwCellIngress<P>;

pub struct BwReplayCellEgress<P, Q>
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
    config_rx: mpsc::UnboundedReceiver<BwReplayCellConfig<P, Q>>,
    send_timer: Timer,
    change_timer: Timer,
    state: AtomicI32,
    notify_rx: Option<tokio::sync::broadcast::Receiver<crate::control::RattanNotify>>,
    started: bool,
}

impl<P, Q> BwReplayCellEgress<P, Q>
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

    fn set_config(&mut self, config: BwReplayCellConfig<P, Q>) {
        if let Some(trace_config) = config.trace_config {
            debug!("Set inner trace config");
            self.trace = trace_config.into_model();
            let now = Instant::now();
            if self.next_change(now).is_none() {
                // handle null trace outside this function
                tracing::warn!("Setting null trace");
                self.next_change = now;
                // set state to 0 to indicate the trace goes to end and the cell will drop all packets
                self.change_state(0);
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
    fn next_change(&mut self, change_time: Instant) -> Option<()> {
        let next_bw = self.trace.next_bw();
        next_bw.map(|(bandwidth, duration)| {
            self.change_bandwidth(bandwidth, change_time);
            self.next_change = change_time + duration;
            trace!(
                "Bandwidth changed to {:?}, next change after {:?}",
                bandwidth,
                self.next_change - Instant::now()
            );
        })
    }
}

#[async_trait]
impl<P, Q> Egress<P> for BwReplayCellEgress<P, Q>
where
    P: Packet + Send + Sync,
    Q: PacketQueue<P>,
{
    async fn dequeue(&mut self) -> Option<P> {
        // Wait for FirstPacket notify if not started yet
        #[cfg(feature = "first-packet")]
        crate::wait_until_started!(self, FirstPacket);
        #[cfg(not(feature = "first-packet"))]
        crate::wait_until_started!(self, Start);

        // wait until next_available
        loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.change_timer.sleep(self.next_change - Instant::now()) => {
                    if self.next_change(self.next_change).is_none() {
                        debug!("Trace goes to end");
                        self.egress.close();
                        return None;
                    }
                }
               recv_packet = self.egress.recv() => {
                    match recv_packet {
                        Some(new_packet) => {
                            match self.state.load(std::sync::atomic::Ordering::Acquire) {
                                0 => {
                                    return None;
                                }
                                1 => {
                                    return Some(new_packet);
                                }
                                _ => {
                                    self.packet_queue.enqueue(new_packet);
                                }
                            }
                        }
                        None => {
                            // channel closed
                            return None;
                        }
                    }
                }
                _ = self.send_timer.sleep(self.next_available - Instant::now()) => {
                    break;
                }
            }
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
                    if self.next_change(self.next_change).is_none() {
                        debug!("Trace goes to end");
                        self.egress.close();
                        return None;
                    }
                }
                recv_packet = self.egress.recv() => {
                    match recv_packet {
                        Some(new_packet) => {
                            match self.state.load(std::sync::atomic::Ordering::Acquire) {
                                0 => {
                                    return None;
                                }
                                1 => {
                                    return Some(new_packet);
                                }
                                _ => {
                                    self.packet_queue.enqueue(new_packet);
                                    packet = self.packet_queue.dequeue();
                                }
                            }
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
        self.next_available = *TRACE_START_INSTANT.get_or_init(Instant::now);
        self.next_change = *TRACE_START_INSTANT.get_or_init(Instant::now);
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }

    fn set_notify_receiver(
        &mut self,
        notify_rx: tokio::sync::broadcast::Receiver<crate::control::RattanNotify>,
    ) {
        self.notify_rx = Some(notify_rx);
    }
}

#[cfg_attr(
    feature = "serde",
    serde_with::skip_serializing_none,
    derive(Deserialize, Serialize)
)]
pub struct BwReplayCellConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub trace_config: Option<Box<dyn BwTraceConfig>>,
    pub queue_config: Option<Q::Config>,
    pub bw_type: Option<BwType>,
}

impl<P, Q> Clone for BwReplayCellConfig<P, Q>
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

impl<P, Q> BwReplayCellConfig<P, Q>
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

pub struct BwReplayCellControlInterface<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    config_tx: mpsc::UnboundedSender<BwReplayCellConfig<P, Q>>,
}

impl<P, Q> ControlInterface for BwReplayCellControlInterface<P, Q>
where
    P: Packet,
    Q: PacketQueue<P> + 'static,
{
    type Config = BwReplayCellConfig<P, Q>;

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
            info!("Setting trace config");
            #[cfg(feature = "serde")]
            debug!(
                "Trace config: {:?}",
                serde_json::to_string_pretty(_trace_config)
            );
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

pub struct BwReplayCell<P: Packet, Q: PacketQueue<P>> {
    ingress: Arc<BwReplayCellIngress<P>>,
    egress: BwReplayCellEgress<P, Q>,
    control_interface: Arc<BwReplayCellControlInterface<P, Q>>,
}

impl<P, Q> Cell<P> for BwReplayCell<P, Q>
where
    P: Packet + Send + Sync + 'static,
    Q: PacketQueue<P> + 'static,
{
    type IngressType = BwReplayCellIngress<P>;
    type EgressType = BwReplayCellEgress<P, Q>;
    type ControlInterfaceType = BwReplayCellControlInterface<P, Q>;

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

impl<P, Q> BwReplayCell<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new<BT: Into<Option<BwType>>>(
        trace: Box<dyn BwTrace>,
        packet_queue: Q,
        bw_type: BT,
    ) -> Result<BwReplayCell<P, Q>, Error> {
        debug!("New BwReplayCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(BwReplayCell {
            ingress: Arc::new(BwReplayCellIngress { ingress: rx }),
            egress: BwReplayCellEgress {
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
                state: AtomicI32::new(0),
                notify_rx: None,
                started: false,
            },
            control_interface: Arc::new(BwReplayCellControlInterface { config_tx }),
        })
    }
}
