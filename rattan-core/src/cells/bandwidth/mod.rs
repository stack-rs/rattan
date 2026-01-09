use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use netem_trace::{model::BwTraceConfig, Bandwidth, BwTrace, Delay};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::Instant;
use tracing::{debug, info, trace};

use super::{
    AtomicCellState, Cell, CellState, ControlInterface, CurrentConfig, Egress, Ingress, Packet,
    LARGE_DURATION, TRACE_START_INSTANT,
};
use crate::cells::bandwidth::queue::AQM;
use crate::error::Error;
use crate::metal::timer::Timer;

pub mod queue;

use queue::PacketQueue;

pub const MAX_BANDWIDTH: Bandwidth = Bandwidth::from_bps(u64::MAX);

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
    fn enqueue(&self, packet: P) -> Result<(), Error> {
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
    packet_queue: AQM<Q, P>,
    next_available: Instant,
    config_rx: mpsc::UnboundedReceiver<BwCellConfig<P, Q>>,
    timer: Timer,
    state: AtomicCellState,
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

    /// If and only if there is enough bandwidth to send a packet which is enqueued into a 0-sized
    /// queue and thus being returnd, this function returns `Some`. When this happens, the caller
    /// should send out the packet immediately.
    #[inline(always)]
    #[must_use]
    fn enqueue_packet(&mut self, new_packet: P) -> Option<P> {
        if let Some(packet_to_drop) = self.packet_queue.enqueue(new_packet) {
            let timestamp = packet_to_drop.get_timestamp();
            if timestamp >= self.next_available {
                self.next_available = timestamp
                    + transfer_time(packet_to_drop.l3_length(), self.bandwidth, self.bw_type);
                return Some(packet_to_drop);
            }
        }
        None
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
                _ = self.timer.sleep(self.next_available - Instant::now()) => {
                    break;
                }
                // `new_packet` can be None only if `self.egress` is closed.
                // The new_packet's timestamp is ealier than self.next_available.
                new_packet = self.egress.recv() => {
                    let new_packet = crate::check_cell_state!(self.state, new_packet?);
                    if let Some(packet) = self.enqueue_packet(new_packet){
                        return Some(packet);
                    }
                }
            }
        }

        while self.packet_queue.need_more_packets(self.next_available) {
            match self.egress.try_recv() {
                Ok(new_packet) => {
                    let new_packet = crate::check_cell_state!(self.state, new_packet);
                    if let Some(packet) = self.enqueue_packet(new_packet) {
                        return Some(packet);
                    }
                }
                Err(TryRecvError::Empty) => {
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    return None;
                }
            }
        }
        // Here, either:
        //    1) No more packets can be retrived from egress, or
        //    2) A packet, that should enter the queue after `self.next_available` is seen.
        // Thus the `dequeue_at()` see a correct queue, containing any packet that should
        // enter the AQM at `self.next_available`.
        let mut packet = self.packet_queue.dequeue_at(self.next_available);

        while packet.is_none() {
            // the queue is empty, wait for the next packet
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                // `new_packet` can be None only if `self.egress` is closed.
                new_packet = self.egress.recv() => {
                    let new_packet = crate::check_cell_state!(self.state, new_packet?);
                    let timestamp = new_packet.get_timestamp();
                    if let Some(packet) = self.enqueue_packet(new_packet){
                        return Some(packet);
                    }
                    packet = self.packet_queue.dequeue_at(timestamp);
                }
            }
        }

        // send the packet
        let mut packet = packet.unwrap();
        let transfer_time = transfer_time(packet.l3_length(), self.bandwidth, self.bw_type);
        if packet.get_timestamp() >= self.next_available {
            // the packet arrives after next_available
            self.next_available = packet.get_timestamp() + transfer_time;
        } else {
            // the packet arrives before next_available and now >= self.next_available
            packet.delay_until(self.next_available);
            self.next_available += transfer_time;
        }
        Some(packet)
    }

    fn change_state(&self, state: CellState) {
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
                packet_queue: AQM::new(packet_queue),
                next_available: Instant::now(),
                config_rx,
                timer: Timer::new()?,
                state: AtomicCellState::new(CellState::Drop),
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
    packet_queue: AQM<Q, P>,
    current_bandwidth: CurrentConfig<Bandwidth>,
    next_available: Instant,
    next_change: Instant,
    config_rx: mpsc::UnboundedReceiver<BwReplayCellConfig<P, Q>>,
    send_timer: Timer,
    change_timer: Timer,
    state: AtomicCellState,
    notify_rx: Option<tokio::sync::broadcast::Receiver<crate::control::RattanNotify>>,
    started: bool,
}

impl<P, Q> BwReplayCellEgress<P, Q>
where
    P: Packet + Send + Sync,
    Q: PacketQueue<P>,
{
    fn change_bandwidth(&mut self, bandwidth: Bandwidth, change_time: Instant) {
        let last_bandwidth = self
            .current_bandwidth
            .update_get_last(bandwidth, change_time);

        trace!(
            "Changing bandwidth from{:?} to {:?} (should at {:?} ago)",
            last_bandwidth,
            bandwidth,
            change_time.elapsed()
        );

        let last_bandwidth = last_bandwidth.unwrap_or(&bandwidth);
        trace!(
            "Previous next_available distance: {:?}",
            self.next_available - change_time
        );

        self.next_available = if bandwidth == Bandwidth::from_bps(0) {
            Instant::now() + LARGE_DURATION
        } else {
            change_time
                + (self.next_available - change_time)
                    .mul_f64(last_bandwidth.as_bps() as f64 / bandwidth.as_bps() as f64)
        };
        trace!(
            "Now next_available distance: {:?}",
            self.next_available - change_time
        );
    }

    fn set_config(&mut self, config: BwReplayCellConfig<P, Q>) {
        if let Some(trace_config) = config.trace_config {
            debug!("Set inner trace config");
            self.trace = trace_config.into_model();
            let now = Instant::now();
            if !self.update_bw(now) {
                // handle null trace outside this function
                tracing::warn!("Setting null trace");
                self.next_change = now;
                // set state to 0 to indicate the trace goes to end and the cell will drop all packets
                self.change_state(CellState::Drop);
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

    /// If the trace does not go to end and a new config was set, returns true and update self.next_change.
    /// If the trace goes to end, returns false, returns false and disable self.next_change. In such case,
    /// the config is not updated so that the latest value is used.
    fn update_bw(&mut self, change_time: Instant) -> bool {
        if let Some((bandwidth, duration)) = self.trace.next_bw() {
            self.change_bandwidth(bandwidth, change_time);
            self.next_change = change_time + duration;
            // #[cfg(test)]
            eprintln!(
                "Bandwidth changed to {:?}, next change after {:?}. now {:?}",
                bandwidth,
                self.next_change - Instant::now(),
                Instant::now(),
            );
            true
        } else {
            debug!("Trace goes to end in DelayReplay Cell");
            self.next_change = change_time + LARGE_DURATION;
            false
        }
    }

    /// If and only if there is enough bandwidth to send a packet which is enqueued into a 0-sized
    /// queue and thus being returnd, this function returns `Some`. When this happens, the caller
    /// should send out the packet immediately.
    #[inline(always)]
    fn enqueue_packet(&mut self, new_packet: P) -> Option<P> {
        if let Some(packet_to_drop) = self.packet_queue.enqueue(new_packet) {
            let timestamp = packet_to_drop.get_timestamp();
            // If the packet queue is 0-sized, yet there is actually enough
            // bandwidth to directly send it out, do so.
            while self.next_change <= timestamp {
                self.update_bw(self.next_change);
            }
            if timestamp >= self.next_available {
                let transfer_time = self
                    .current_bandwidth
                    .get_current(timestamp)
                    .map(|bw| transfer_time(packet_to_drop.l3_length(), *bw, self.bw_type));
                self.next_available = timestamp + transfer_time.unwrap_or_default();
                return Some(packet_to_drop);
            }
        }
        None
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
                _ = self.change_timer.sleep(self.next_change - Instant::now()),
                    if self.next_change <= self.next_available => {
                    self.update_bw(self.next_change);
                }
                _ = self.send_timer.sleep(self.next_available - Instant::now()),
                    if self.next_change > self.next_available => {
                    break;
                }
                // `new_packet` can be None only if `self.egress` is closed.
                new_packet = self.egress.recv() => {
                    let new_packet = crate::check_cell_state!(self.state, new_packet?);
                    if let Some(packet) = self.enqueue_packet(new_packet){
                        return Some(packet);
                    }
                }

            }
        }

        while self.packet_queue.need_more_packets(self.next_available) {
            match self.egress.try_recv() {
                Ok(new_packet) => {
                    let new_packet = crate::check_cell_state!(self.state, new_packet);
                    if let Some(packet) = self.enqueue_packet(new_packet) {
                        return Some(packet);
                    }
                }
                Err(TryRecvError::Empty) => {
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    return None;
                }
            }
        }

        // Here, either:
        //    1) No more packets can be retrived from egress, or
        //    2) A packet, that should enter the queue after `self.next_available` is seen.
        // Thus the `dequeue_at()` see a correct queue, containing any packet that should
        // enter the AQM at `self.next_available`.
        let mut packet = self.packet_queue.dequeue_at(self.next_available);

        while packet.is_none() {
            // the queue is empty, wait for the next packet
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.change_timer.sleep(self.next_change - Instant::now()) => {
                    self.update_bw(self.next_change);
                }
                // `new_packet` can be None only if `self.egress` is closed.
                new_packet = self.egress.recv() => {
                    let new_packet = crate::check_cell_state!(self.state, new_packet?);
                    let timestamp = new_packet.get_timestamp();
                    if let Some(packet) = self.enqueue_packet(new_packet){
                        return Some(packet);
                    }
                    packet = self.packet_queue.dequeue_at(timestamp);
                }
            }
        }

        // send the packet
        let mut packet = packet.unwrap();
        let timestamp = packet.get_timestamp();

        let transfer_time = self
            .current_bandwidth
            .get_current(timestamp)
            .map(|bw| transfer_time(packet.l3_length(), *bw, self.bw_type));

        // release the packet immediately (aka infinity bandwidth) when no avaiable bandwidth has been set.
        let transfer_time = transfer_time.unwrap_or_default();

        if timestamp >= self.next_available {
            // the packet arrives after next_available
            self.next_available = timestamp + transfer_time;
        } else {
            // the packet arrives before next_available and now >= self.next_available
            packet.delay_until(self.next_available);
            self.next_available += transfer_time;
        }
        Some(packet)
    }

    // This must be called before any dequeue
    fn reset(&mut self) {
        self.next_available = *TRACE_START_INSTANT.get_or_init(Instant::now);
        self.next_change = *TRACE_START_INSTANT.get_or_init(Instant::now);
        eprintln!("Reset to {:?}", self.next_change);
    }

    fn change_state(&self, state: CellState) {
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
                packet_queue: AQM::new(packet_queue),
                current_bandwidth: CurrentConfig::default(),
                next_available: Instant::now(),
                next_change: Instant::now(),
                config_rx,
                send_timer: Timer::new()?,
                change_timer: Timer::new()?,
                state: AtomicCellState::new(CellState::Drop),
                notify_rx: None,
                started: false,
            },
            control_interface: Arc::new(BwReplayCellControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::cells::bandwidth::queue::{DropTailQueue, DropTailQueueConfig};

    use super::*;
    use crate::cells::{StdPacket, TestPacket};
    use netem_trace::model::{RepeatedBwPatternConfig, StaticBwConfig};
    use tracing::{info, span, Level};

    #[test_log::test]
    fn zero_buffer_bw() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_zero_buffer_bw").entered();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let _guard = rt.enter();

        // A 256B packet needs 80ms transmission time.
        let bandwidth = Bandwidth::from_bps(25600);
        let packet_queue =
            DropTailQueue::new(DropTailQueueConfig::new(None, 0, BwType::NetworkLayer));
        let cell = BwCell::new(bandwidth, packet_queue, BwType::NetworkLayer)?;

        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        let mut logical_send_time = Instant::now();
        let logical_start = logical_send_time;
        for i in 0..16 {
            ingress.enqueue(TestPacket::<StdPacket>::with_timestamp(
                &[i; 256],
                logical_send_time,
            ))?;
            logical_send_time += Duration::from_millis(25);
        }

        for i in 0..4 {
            let received = rt.block_on(async { egress.dequeue().await }).unwrap();
            assert_eq!(received.packet.buf[0], i * 4);
            assert_eq!(Duration::ZERO, received.delay());
            info!(
                "Packet {} sent at {:?} has been delayed by {:?} ",
                received.packet.buf[0],
                received
                    .packet
                    .get_timestamp()
                    .duration_since(logical_start),
                received.delay()
            )
        }

        Ok(())
    }

    #[test_log::test]
    fn zero_buffer_bw_replay() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_zero_buffer_bw_replay").entered();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let _guard = rt.enter();

        let bandwidth_trace = RepeatedBwPatternConfig::new()
            .pattern(vec![
                // A 256B packet needs 80ms transmission time for 25.6Kbps,
                Box::new(StaticBwConfig {
                    bw: Some(Bandwidth::from_bps(25600)),
                    duration: Some(Duration::from_millis(400)),
                }) as Box<dyn BwTraceConfig>,
                // A 256B packet needs 160ms transmission time for 25.6Kbps,
                Box::new(StaticBwConfig {
                    bw: Some(Bandwidth::from_bps(12800)),
                    duration: Some(Duration::from_millis(40000)),
                }) as Box<dyn BwTraceConfig>,
            ])
            .build();

        let packet_queue =
            DropTailQueue::new(DropTailQueueConfig::new(None, 0, BwType::NetworkLayer));
        let cell = BwReplayCell::new(
            Box::new(bandwidth_trace),
            packet_queue,
            BwType::NetworkLayer,
        )?;

        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        let mut logical_send_time = Instant::now();
        let logical_start = logical_send_time;
        for i in 0..16 {
            ingress.enqueue(TestPacket::<StdPacket>::with_timestamp(
                &[i; 256],
                logical_send_time,
            ))?;
            logical_send_time += Duration::from_millis(50);
        }

        for i in [0, 2, 4, 6, 8, 12] {
            let received = rt.block_on(async { egress.dequeue().await }).unwrap();
            assert_eq!(received.packet.buf[0], i);
            assert_eq!(Duration::ZERO, received.delay());
            info!(
                "Packet {} sent at {:?} has been delayed by {:?} ",
                received.packet.buf[0],
                received
                    .packet
                    .get_timestamp()
                    .duration_since(logical_start),
                received.delay()
            )
        }

        Ok(())
    }
}
