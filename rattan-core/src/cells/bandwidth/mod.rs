use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use netem_trace::{model::BwTraceConfig, Bandwidth, BwTrace, Delay};
use rattan_env::timer::Timer;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{
    AtomicCellState, Cell, CellState, ControlInterface, Egress, Ingress, Packet, TimedConfig,
    LARGE_DURATION, TRACE_START_INSTANT,
};
use crate::cells::bandwidth::queue::AQM;
#[cfg(test)]
use crate::cells::relative_time;
use crate::error::Error;

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

#[derive(Clone, Debug, derive_more::Deref, derive_more::DerefMut)]
struct Transmitting<P: Packet> {
    #[deref]
    #[deref_mut]
    pub packet: P,
    /// At the time of `packet.get_timestamp()`, what portion of the packet has not been partially transmitted
    /// [0,1]
    unsent_portion: f64,
}

impl<P: Packet> Transmitting<P> {
    fn start(mut packet: P, start_time: Instant) -> Self {
        packet.delay_until(start_time);
        Self {
            packet,
            unsent_portion: 1.0f64,
        }
    }

    /// Update the unsent_portion
    /// given that the sending rate is `last_rate` during the period [`packet.get_timestamp()` , current)
    fn update(&mut self, last_rate: Bandwidth, bw_type: BwType, current: Instant) {
        let last_time = self.get_timestamp();
        self.delay_until(current);
        if last_rate.is_zero() {
            return;
        }
        let total_packet_time = transfer_time(self.l3_length(), last_rate, bw_type);
        let sent_for = current.duration_since(last_time);

        let sent_portion = sent_for.div_duration_f64(total_packet_time);
        self.unsent_portion = f64::max(0.0f64, self.unsent_portion - sent_portion);
    }

    fn finish_time(&self, rate: Bandwidth, bw_type: BwType) -> Instant {
        // A packet whose transmission is already complete leaves right away, even if the
        // bandwidth has just dropped to zero.
        if self.unsent_portion == 0.0 {
            return self.get_timestamp();
        }
        let transfer_time = if rate.is_zero() {
            LARGE_DURATION
        } else {
            transfer_time(self.l3_length(), rate, bw_type).mul_f64(self.unsent_portion)
        };
        self.get_timestamp() + transfer_time
    }

    fn take(self) -> P {
        self.packet
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
    transmitting_packet: Option<Transmitting<P>>,
}

impl<P, Q> BwCellEgress<P, Q>
where
    P: Packet + Send + Sync,
    Q: PacketQueue<P>,
{
    fn set_config(&mut self, config: BwCellConfig<P, Q>) {
        if let Some(bandwidth) = config.bandwidth {
            if let Some(transmitting_packet) = self.transmitting_packet.as_mut() {
                let change_time = Instant::now();
                transmitting_packet.update(self.bandwidth, self.bw_type, change_time);
                let next_available = transmitting_packet.finish_time(bandwidth, self.bw_type);
                #[cfg(test)]
                tracing::debug!(
                    "Packet scheduled to be sent at {:?} is rescheduled to {:?}",
                    relative_time(self.next_available),
                    relative_time(next_available)
                );
                self.next_available = next_available;
            }
            self.bandwidth = bandwidth;
        }
        if let Some(queue_config) = config.queue_config {
            tracing::debug!(?queue_config, "Set inner queue config:");
            self.packet_queue.configure(queue_config);
        }
        if let Some(bw_type) = config.bw_type {
            tracing::debug!(?bw_type, "Set inner bw_type:");
            self.bw_type = bw_type;
        }
    }

    #[inline(always)]
    fn enqueue_packet(&mut self, new_packet: P) {
        #[cfg(test)]
        tracing::debug!(
            "Packet with timestamp {:?} is enqueued at {:?}",
            relative_time(new_packet.get_timestamp()),
            relative_time(Instant::now())
        );
        if let Some(packet_to_drop) = self.packet_queue.enqueue(new_packet) {
            // If the `self.transmitting_packet` is Some, the `packet_to_drop` is dropped.
            self.set_transmitting_packet(packet_to_drop);
        }
    }

    #[inline(always)]
    fn set_transmitting_packet(&mut self, packet: P) {
        if self.transmitting_packet.is_none() {
            let start_transmission = self.next_available.max(packet.get_timestamp());
            let transmitting_packet = Transmitting::start(packet, start_transmission);
            self.next_available = transmitting_packet.finish_time(self.bandwidth, self.bw_type);
            #[cfg(test)]
            tracing::debug!(
                "Packet at {:?} is going to be delayed until {:?}",
                relative_time(transmitting_packet.get_timestamp()),
                relative_time(self.next_available)
            );
            self.transmitting_packet = transmitting_packet.into();
        } else {
            #[cfg(test)]
            tracing::debug!(
                "Packet at {:?} is going to be dropped",
                relative_time(packet.get_timestamp())
            );
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
        let mut first_payload_packet = None;
        if cfg!(feature = "first-payload") && TRACE_START_INSTANT.get().is_none() {
            let packet = self.egress.recv().await?;
            if TRACE_START_INSTANT.get().is_none() {
                return Some(packet);
            }
            tracing::info!(
                target = "first-payload",
                "Special handling: first payload packet of L3 length {}",
                packet.l3_length()
            );
            first_payload_packet = Some(packet);
        }
        // Wait for Start notify if not started yet
        crate::wait_until_started!(self, Start);

        // Wait for time
        loop {
            if let Some(first_payload_packet) = first_payload_packet.take() {
                // As this is the first packet the cell actually handles,
                // we can enqueue it directly without waiting for `next_available`.
                self.enqueue_packet(first_payload_packet);
                break;
            }
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                // `new_packet` can be None only if `self.egress` is closed.
                // Do not return None here, as there may be some packets in the AQM.
                Some(new_packet) = self.egress.recv() => {
                    // XXX: If state change from `Normal` to others, the packets in the AQM may never be sent.
                    let new_packet = crate::check_cell_state!(self.state, new_packet);
                    if new_packet.get_timestamp() < self.next_available {
                        self.enqueue_packet(new_packet);
                    } else {
                        self.enqueue_packet(new_packet);
                        // Assert that: new_packet.get_timestamp() >= Instant::now()
                        break;
                    }
                }
                _ = self.timer.sleep(self.next_available - Instant::now()) => {
                    break;
                }
            }
        }

        // Here, either:
        //    1) No more packets can be retrieved from egress, or
        //    2) A packet, that should enter the queue after `self.next_available` is seen.
        // Thus, the `dequeue_at()` sees a correct queue, containing any packet that should
        // enter the AQM at `self.next_available`.

        let packet_to_send = self.transmitting_packet.take().map(|mut p| {
            p.delay_until(self.next_available);
            p.take()
        });
        if let Some(next_packet) = self.packet_queue.dequeue_at(self.next_available) {
            // Here, self.transmitting_packet can not be Some, thus the next_packet would not be dropped.
            self.set_transmitting_packet(next_packet);
        }
        if packet_to_send.is_some() {
            return packet_to_send;
        }

        // Wait for packet to send.
        // If this loop is entered, the packet_queue in the AQM is empty, and `Instant::now()` >= `self.next_available`.
        while self.transmitting_packet.is_none() {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }

                Ok(time) = self.timer.sleep_until(self.packet_queue.next_call_time()) => {
                    if let Some(next_packet) = self.packet_queue.dequeue_at(time) {
                        // Here, self.transmitting_packet can not be Some, thus the next_packet would not be dropped.
                        self.set_transmitting_packet(next_packet);
                    }
                }

                // If we enter this branch and got a new packet, whose logical timestamp is `np`, we assume that `np` <= Instant::now().
                //   A) If the inbound buffer AQM is empty (in such case, self.packet_queue.next_call_time() is in 10 years),
                //     Whenever we get a packet, we should send it as soon as possible. As the two buffers in the AQM are
                //     all empty, the `dequeue_at(timestamp)` shall just return the newly received packet. We set that as
                //     the `self.transmitting_packet`, during which the self.next_available is updated based on the timestamp
                //     of the newly received packet.
                //   B) Otherwise, assume that there are some packets in the inbound buffer of the AQM, and the packet at its head has a timestamp of `hp`.
                //     We know that :
                //     1) `np` >= `hp` , as long as the logical timestamp of packets is non-descending.
                //     2) `Instant::now()` >= `np`, as we shall not receive a packet prior to its logical timestamp
                //     3) `Instant::now()` < `hp`, or we should have entered the `select!` branch above, who `sleeps_until` `hp`.
                //     Join them together, and we got `Instant::now()` >= `np` >= `hp` > `Instant::now()`, which is self-conflicting.
                //     Thus, this condition is impossible.
                // In conclusion, A) is the only possible condition when this branch is entered.
                new_packet = self.egress.recv() => {
                    // XXX: If state change from `Normal` to others, the packets in the AQM may never be sent.
                    // `new_packet` can be None only if `self.egress` is closed.`
                    // If egress is closed here, then no further packets can be enqueued, so return None with `?`.
                    let new_packet = crate::check_cell_state!(self.state, new_packet?);
                    let timestamp = new_packet.get_timestamp();
                    self.enqueue_packet(new_packet);
                    if let Some(next_packet) = self.packet_queue.dequeue_at(timestamp) {
                        // Here, self.transmitting_packet can not be Some, thus the next_packet would not be dropped.
                        self.set_transmitting_packet(next_packet);
                    }
                }
            }
        }

        self.transmitting_packet
            .take_if(|_| self.next_available <= Instant::now())
            .map(|mut p| {
                p.delay_until(self.next_available);
                p.take()
            })
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
            tracing::info!("Setting bandwidth to: {:?}", bandwidth);
        }
        if let Some(queue_config) = config.queue_config.as_ref() {
            tracing::info!("Setting queue config to: {:?}", queue_config);
        }
        if let Some(bw_type) = config.bw_type {
            tracing::info!("Setting bw_type to: {:?}", bw_type);
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
        tracing::debug!("New BwCell");
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
                transmitting_packet: None,
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
    current_bandwidth: TimedConfig<Bandwidth>,
    next_available: Instant,
    next_change: Instant,
    config_rx: mpsc::UnboundedReceiver<BwReplayCellConfig<P, Q>>,
    send_timer: Timer,
    change_timer: Timer,
    state: AtomicCellState,
    notify_rx: Option<tokio::sync::broadcast::Receiver<crate::control::RattanNotify>>,
    started: bool,
    transmitting_packet: Option<Transmitting<P>>,
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

        tracing::debug!(
            "Changing bandwidth from{:?} to {:?} (should at {:?} ago)",
            last_bandwidth,
            bandwidth,
            change_time.elapsed()
        );

        let Some(transmitting_packet) = self.transmitting_packet.as_mut() else {
            // If the bw change does not happen during a packet's logical transmission period (i.e. from it being
            // set as the trasmitting packet to it being logical sent), nothing to do.
            return;
        };

        let last_bandwidth = last_bandwidth.cloned().unwrap_or(bandwidth);

        transmitting_packet.update(last_bandwidth, self.bw_type, change_time);
        let next_available = transmitting_packet.finish_time(bandwidth, self.bw_type);
        #[cfg(test)]
        tracing::debug!(
            "Packet scheduled to be sent at {:?} is rescheduled to {:?}",
            relative_time(self.next_available),
            relative_time(next_available)
        );
        self.next_available = next_available;
    }

    fn set_config(&mut self, config: BwReplayCellConfig<P, Q>) {
        if let Some(trace_config) = config.trace_config {
            tracing::debug!("Set inner trace config");
            self.trace = trace_config.into_model();
            let now = Instant::now();
            if !self.update_bw(now) {
                // handle null trace outside this function
                tracing::debug!("Setting null trace");
                self.next_change = now;
                // set state to 0 to indicate the trace goes to end and the cell will drop all packets
                self.change_state(CellState::Drop);
            }
        }
        if let Some(queue_config) = config.queue_config {
            tracing::debug!(?queue_config, "Set inner queue config:");
            self.packet_queue.configure(queue_config);
        }
        if let Some(bw_type) = config.bw_type {
            tracing::debug!(?bw_type, "Set inner bw_type:");
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
            #[cfg(test)]
            tracing::debug!(
                "Bandwidth changed to {:?}, next change after {:?}. now {:?}",
                bandwidth,
                self.next_change - Instant::now(),
                relative_time(Instant::now()),
            );
            true
        } else {
            tracing::debug!("Trace goes to end in DelayReplay Cell");
            self.next_change = change_time + LARGE_DURATION;
            false
        }
    }

    #[inline(always)]
    fn enqueue_packet(&mut self, new_packet: P) {
        #[cfg(test)]
        tracing::debug!(
            "Packet with timestamp {:?} is enqueued at {:?}",
            relative_time(new_packet.get_timestamp()),
            relative_time(Instant::now())
        );
        if let Some(packet_to_drop) = self.packet_queue.enqueue(new_packet) {
            // If the `self.transmitting_packet` is Some, the `packet_to_drop` is dropped.
            self.set_transmitting_packet(packet_to_drop);
        }
    }

    #[inline(always)]
    fn set_transmitting_packet(&mut self, packet: P) {
        if self.transmitting_packet.is_none() {
            let start_transmission = self.next_available.max(packet.get_timestamp());
            let transmitting_packet = Transmitting::start(packet, start_transmission);
            let bandwidth = self
                .current_bandwidth
                .get_at_timestamp(start_transmission)
                .cloned()
                .unwrap_or(Bandwidth::ZERO);
            self.next_available = transmitting_packet.finish_time(bandwidth, self.bw_type);

            #[cfg(test)]
            tracing::debug!(
                "Packet at {:?} is going to be delayed until {:?}",
                relative_time(transmitting_packet.get_timestamp()),
                relative_time(self.next_available)
            );
            self.transmitting_packet = transmitting_packet.into();
        } else {
            #[cfg(test)]
            tracing::debug!(
                "Packet at {:?} is going to be dropped",
                relative_time(packet.get_timestamp())
            );
        }
    }
}

#[async_trait]
impl<P, Q> Egress<P> for BwReplayCellEgress<P, Q>
where
    P: Packet + Send + Sync,
    Q: PacketQueue<P>,
{
    async fn dequeue(&mut self) -> Option<P> {
        let mut first_payload_packet = None;
        if cfg!(feature = "first-payload") && TRACE_START_INSTANT.get().is_none() {
            let packet = self.egress.recv().await?;
            if TRACE_START_INSTANT.get().is_none() {
                return Some(packet);
            }
            tracing::info!(
                target = "first-payload",
                "Special handling: first payload packet of L3 length {}",
                packet.l3_length()
            );
            first_payload_packet = Some(packet);
        }
        // Wait for FirstPacket notify if not started yet
        #[cfg(feature = "first-packet")]
        crate::wait_until_started!(self, FirstPacket);
        #[cfg(not(feature = "first-packet"))]
        crate::wait_until_started!(self, Start);

        // wait until next_available
        loop {
            if let Some(first_payload_packet) = first_payload_packet.take() {
                // As this is the first packet the cell actually handles,
                // we can enqueue it directly without waiting for `next_available`.
                self.update_bw(self.next_change);
                self.enqueue_packet(first_payload_packet);
                break;
            }
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.change_timer.sleep(self.next_change - Instant::now()), if self.next_change <= self.next_available => {
                    self.update_bw(self.next_change);
                }
                // `new_packet` can be None only if `self.egress` is closed.
                // Do not return None here, as there may be some packets in the AQM.
                Some(new_packet) = self.egress.recv() => {
                    // XXX: If state change from `Normal` to others, the packets in the AQM may never be sent.
                    let new_packet = crate::check_cell_state!(self.state, new_packet);
                    if new_packet.get_timestamp() < self.next_available {
                        self.enqueue_packet(new_packet);
                    } else {
                        self.enqueue_packet(new_packet);
                        // Assert that: new_packet.get_timestamp() >= Instant::now()
                        break;
                    }
                }
                _ = self.send_timer.sleep(self.next_available - Instant::now()), if self.next_change > self.next_available => {
                    break;
                }
            }
        }

        // Here, either:
        //    1) No more packets can be retrieved from egress, or
        //    2) A packet, that should enter the queue after `self.next_available` is seen.
        // Thus, the `dequeue_at()` sees a correct queue, containing any packet that should
        // enter the AQM at `self.next_available`.

        let packet_to_send = self.transmitting_packet.take().map(|mut p| {
            p.delay_until(self.next_available);
            p.take()
        });
        if let Some(next_packet) = self.packet_queue.dequeue_at(self.next_available) {
            // Here, self.transmitting_packet can not be Some, thus the next_packet would not be dropped.
            self.set_transmitting_packet(next_packet);
        }
        if packet_to_send.is_some() {
            return packet_to_send;
        }

        // Wait for packet to send.
        // If this loop is entered, the packet_queue in the AQM is empty, and `Instant::now()` >= `self.next_available`.
        while self.transmitting_packet.is_none() {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.change_timer.sleep(self.next_change - Instant::now()) => {
                    self.update_bw(self.next_change);
                }
                Ok(time) = self.send_timer.sleep_until(self.packet_queue.next_call_time()) => {
                     if let Some(next_packet) = self.packet_queue.dequeue_at(time) {
                        // Here, self.transmitting_packet can not be Some, thus the next_packet would not be dropped.
                        self.set_transmitting_packet(next_packet);
                    }
                }

                // If we enter this branch and got a new packet, whose logical timestamp is `np`, we assume that `np` <= Instant::now().
                //   A) If the inbound buffer AQM is empty (in such case, self.packet_queue.next_call_time() is in 10 years),
                //     Whenever we get a packet, we should send it as soon as possible. As the two buffers in the AQM are
                //     all empty, the `dequeue_at(timestamp)` shall just return the newly received packet. We set that as
                //     the `self.transmitting_packet`, during which the self.next_available is updated based on the timestamp
                //     of the newly received packet.
                //   B) Otherwise, assume that there are some packets in the inbound buffer of the AQM, and the packet at its head has a timestamp of `hp`.
                //     We know that :
                //     1) `np` >= `hp` , as long as the logical timestamp of packets is non-descending.
                //     2) `Instant::now()` >= `np`, as we shall not receive a packet prior to its logical timestamp
                //     3) `Instant::now()` < `hp`, or we should have entered the `select!` branch above, who `sleeps_until` `hp`.
                //     Join them together, and we got `Instant::now()` >= `np` >= `hp` > `Instant::now()`, which is self-conflicting.
                //     Thus, this condition is impossible.
                // In conclusion, A) is the only possible condition when this branch is entered.
                new_packet = self.egress.recv() => {
                    // XXX: If state change from `Normal` to others, the packets in the AQM may never be sent.
                    // `new_packet` can be None only if `self.egress` is closed.
                    //  If egress is closed here, then no further packets can be enqueued, so return None with `?`.
                    let new_packet = crate::check_cell_state!(self.state, new_packet?);
                    let timestamp = new_packet.get_timestamp();
                    self.enqueue_packet(new_packet);
                    if let Some(next_packet) = self.packet_queue.dequeue_at(timestamp) {
                        // Here, self.transmitting_packet can not be Some, thus the next_packet would not be dropped.
                        self.set_transmitting_packet(next_packet);
                    }
                }
            }
        }

        self.transmitting_packet
            .take_if(|_| self.next_available <= Instant::now())
            .map(|mut p| {
                p.delay_until(self.next_available);
                p.take()
            })
    }

    // This must be called before any dequeue
    fn reset(&mut self) {
        self.next_available = *TRACE_START_INSTANT.get_or_init(Instant::now);
        self.next_change = *TRACE_START_INSTANT.get_or_init(Instant::now);
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
            tracing::info!("Setting trace config");
            #[cfg(feature = "serde")]
            tracing::debug!(
                "Trace config: {:?}",
                serde_json::to_string_pretty(_trace_config)
            );
        }
        if let Some(queue_config) = config.queue_config.as_ref() {
            tracing::info!("Setting queue config to: {:?}", queue_config);
        }
        if let Some(bw_type) = config.bw_type {
            tracing::info!("Setting bw_type to: {:?}", bw_type);
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
        tracing::debug!("New BwReplayCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(BwReplayCell {
            ingress: Arc::new(BwReplayCellIngress { ingress: rx }),
            egress: BwReplayCellEgress {
                egress: tx,
                bw_type: bw_type.into().unwrap_or_default(),
                trace,
                packet_queue: AQM::new(packet_queue),
                current_bandwidth: TimedConfig::default(),
                next_available: Instant::now(),
                next_change: Instant::now(),
                config_rx,
                send_timer: Timer::new()?,
                change_timer: Timer::new()?,
                state: AtomicCellState::new(CellState::Drop),
                notify_rx: None,
                started: false,
                transmitting_packet: None,
            },
            control_interface: Arc::new(BwReplayCellControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::Duration;

    use netem_trace::model::{RepeatedBwPatternConfig, StaticBwConfig};
    use rstest::rstest;
    use tracing::{span, Level};

    use super::*;
    use crate::cells::bandwidth::queue::{
        DropTailQueue, DropTailQueueConfig, InfiniteQueue, InfiniteQueueConfig,
    };
    use crate::cells::{StdPacket, TestPacket};

    async fn pacing_send<const SIZE: usize, P: Packet>(
        packet_cnt: u8,
        interval_ms: u64,
        mut logical_send_time: Instant,
        ingress: Arc<BwCellIngress<TestPacket<P>>>,
    ) {
        for i in 0..packet_cnt {
            tokio::time::sleep_until(logical_send_time).await;
            ingress
                .enqueue(TestPacket::with_timestamp(&[i; SIZE], logical_send_time))
                .unwrap();
            logical_send_time += Duration::from_millis(interval_ms);
        }
    }

    #[rstest]
    #[case(50)]
    #[case(80)]
    #[case(90)]
    #[test_log::test]
    fn test_send_interval(#[case] send_interval: u64) -> Result<(), Error> {
        let _span = span!(Level::DEBUG, "test_send_interval").entered();

        tracing::info!("Input Packet Interval = {:?}ms", send_interval);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let _guard = rt.enter();

        // A 256B + 14B packet needs 80ms transmission time.
        let bandwidth = Bandwidth::from_bps(25600);

        let packet_queue = InfiniteQueue::new(InfiniteQueueConfig {});
        let cell = BwCell::new(bandwidth, packet_queue, BwType::NetworkLayer)?;

        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        let logical_send_time = Instant::now() + Duration::from_millis(10);
        let logical_start = logical_send_time;

        relative_time(logical_start);

        let _handle = rt.spawn(pacing_send::<{ 256 + 14 }, TestPacket<StdPacket>>(
            16,
            send_interval,
            logical_send_time,
            ingress,
        ));

        let mut actual_receive_time = vec![];
        let mut recv_cnt = 0;
        while recv_cnt < 16 {
            let Some(received) = rt.block_on(async { egress.dequeue().await }) else {
                continue;
            };
            actual_receive_time.push(logical_start.elapsed());
            assert_eq!(received.packet.buf[0] as u64, recv_cnt);
            assert_eq!(
                Duration::from_millis(80 + recv_cnt * 80u64.saturating_sub(send_interval)),
                received.delay()
            );
            tracing::debug!(
                "Packet {} has been delayed by logically {:?}",
                received.packet.buf[0],
                received.delay()
            );
            recv_cnt += 1;
        }

        tracing::info!("Actual receive time {:?}", &actual_receive_time);

        for (i, actual_receive_time) in actual_receive_time.into_iter().enumerate() {
            let delay_ms = actual_receive_time.as_secs_f64() * 1E3;
            let expected_delay_ms =
                80f64 * (i + 1) as f64 + send_interval.saturating_sub(80) as f64 * (i) as f64;
            assert!(expected_delay_ms <= delay_ms + 1.0);
            assert!(expected_delay_ms >= delay_ms - 1.0);
        }

        Ok(())
    }

    fn compare_receive_time(times: Vec<(Duration, Duration)>) {
        let diffs: Vec<_> = times
            .iter()
            .map(|(t1, t2)| t1.as_secs_f64() - t2.as_secs_f64())
            .collect();

        let (actual, expect): (Vec<Duration>, Vec<Duration>) = times.into_iter().unzip();
        tracing::info!(?actual, "Actual Receive Times:");
        tracing::info!(?expect, "Expected Receive Times:");

        for v in diffs {
            assert!(v.abs() <= Duration::from_millis(1).as_secs_f64())
        }
    }

    #[test_log::test]
    fn zero_buffer_bw() -> Result<(), Error> {
        let _span = span!(Level::DEBUG, "test_zero_buffer_bw").entered();

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

        let logical_send_time = Instant::now();
        let logical_start = logical_send_time;
        relative_time(logical_start);

        let _handle = rt.spawn(pacing_send::<{ 256 + 14 }, TestPacket<StdPacket>>(
            16,
            25,
            logical_send_time + Duration::from_millis(10),
            ingress,
        ));

        let mut receive_time = vec![];
        let mut recv_cnt = 0;
        while recv_cnt < 4 {
            let Some(received) = rt.block_on(async { egress.dequeue().await }) else {
                continue;
            };
            receive_time.push((
                logical_start.elapsed(),
                Duration::from_millis(100 * (recv_cnt + 1) - 10),
            ));

            assert_eq!(received.packet.buf[0] as u64, recv_cnt * 4);
            assert_eq!(Duration::from_millis(80), received.delay());
            tracing::info!(
                "Packet {} sent at {:?} has been delayed by {:?} ",
                received.packet.buf[0],
                relative_time(received.packet.get_timestamp()) - received.delay(),
                received.delay()
            );
            recv_cnt += 1;
        }

        compare_receive_time(receive_time);

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

        let logical_send_time = Instant::now();
        let logical_start = logical_send_time;
        relative_time(logical_start);

        let _handle = rt.spawn(pacing_send::<{ 256 + 14 }, TestPacket<StdPacket>>(
            16,
            50,
            logical_send_time + Duration::from_millis(10),
            ingress,
        ));

        let mut expecting = VecDeque::from(vec![
            (0, 80),
            (2, 80),
            (4, 80),
            (6, 80),
            (8, 160),
            (12, 160),
        ]);
        let mut receive_time = vec![];

        while !expecting.is_empty() {
            let Some(received) = rt.block_on(async { egress.dequeue().await }) else {
                continue;
            };
            let (i, expected_delay) = expecting.pop_front().unwrap();
            receive_time.push((
                logical_start.elapsed(),
                Duration::from_millis(10 + i * 50 + expected_delay),
            ));
            assert_eq!(received.packet.buf[0] as u64, i);
            assert_eq!(Duration::from_millis(expected_delay), received.delay());
            tracing::info!(
                "Packet {} sent at {:?} has been delayed by {:?} ",
                received.packet.buf[0],
                relative_time(received.packet.get_timestamp()) - received.delay(),
                received.delay()
            );
        }

        compare_receive_time(receive_time);

        Ok(())
    }

    /// Simulate the departure schedule of an ideal fluid server fed by the rectangular-wave
    /// trace used in [`rectangular_wave_bw_replay`]: `x_kbps` during the first 10ms of each
    /// 20ms period and `y_kbps` during the second 10ms. All `packet_cnt` packets arrive at
    /// the same instant, `arrival_offset` after the trace start instant, so the returned
    /// durations are the expected logical departure delays relative to that arrival instant.
    /// This mirrors exactly what the cell does: a packet in transmission makes progress at
    /// the current rate and is rescheduled with the remaining portion whenever the trace
    /// changes rate.
    fn expected_departure_delays(
        x_kbps: u64,
        y_kbps: u64,
        packet_cnt: usize,
        l3_len: usize,
        arrival_offset: Duration,
    ) -> Vec<Duration> {
        assert!(x_kbps > 0 || y_kbps > 0, "the trace must not be all-zero");
        let bw_type = BwType::NetworkLayer;
        let segment = Duration::from_millis(10);

        let mut delays = Vec::with_capacity(packet_cnt);
        // Departure time of the previous packet, relative to the arrival instant.
        let mut departure = Duration::ZERO;

        for _ in 0..packet_cnt {
            // Fraction of the packet that still has to be transmitted.
            let mut remaining = 1.0f64;
            loop {
                // Index of the trace segment the packet is currently in.
                let segment_index = (arrival_offset + departure).as_nanos() / segment.as_nanos();
                // End of that segment, relative to the arrival instant.
                let segment_end = segment * (segment_index as u32 + 1) - arrival_offset;
                let rate_bps = (if segment_index % 2 == 0 {
                    x_kbps
                } else {
                    y_kbps
                }) * 1000;

                if rate_bps == 0 {
                    // No progress is made while the trace has zero bandwidth.
                    departure = segment_end;
                    continue;
                }

                let full_transfer = transfer_time(l3_len, Bandwidth::from_bps(rate_bps), bw_type);
                let served = (segment_end - departure).div_duration_f64(full_transfer);
                if served >= remaining {
                    departure += full_transfer.mul_f64(remaining);
                    break;
                }
                remaining -= served;
                departure = segment_end;
            }
            delays.push(departure);
        }
        delays
    }

    /// A burst of `PACKET_CNT` packets is sent through a `BwReplayCell` whose trace is a
    /// repeated rectangular wave: `x_kbps` for the first 10ms of each 20ms period and
    /// `y_kbps` for the second 10ms. As the cell is configured with an `InfiniteQueue`, the
    /// packets (all arriving at the same instant) should leave according to the departure
    /// schedule of an ideal fluid server fed by the same wave. Both the wall-clock receive
    /// time and the logical departure timestamp are checked.
    #[rstest]
    #[case(100, 100)]
    #[case(100, 200)]
    #[case(100, 0)]
    #[test_log::test]
    fn rectangular_wave_bw_replay(#[case] x_kbps: u64, #[case] y_kbps: u64) -> Result<(), Error> {
        // A 200B L3 payload takes 16ms to transmit at 100kbps and 8ms at 200kbps.
        const L3_LEN: usize = 200;
        const PACKET_CNT: usize = 100;
        const BUFFER_SIZE: usize = L3_LEN + 14; // 14B Ethernet header
        const WALL_CLOCK_TOLERANCE: Duration = Duration::from_millis(2);

        let _span = span!(Level::INFO, "rectangular_wave_bw_replay", x_kbps, y_kbps).entered();
        tracing::info!(
            "Rectangular wave: 10ms at {} kbps, then 10ms at {} kbps",
            x_kbps,
            y_kbps
        );

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let _guard = rt.enter();

        let bandwidth_trace = RepeatedBwPatternConfig::new()
            .pattern(vec![
                Box::new(StaticBwConfig {
                    bw: Some(Bandwidth::from_kbps(x_kbps)),
                    duration: Some(Duration::from_millis(10)),
                }) as Box<dyn BwTraceConfig>,
                Box::new(StaticBwConfig {
                    bw: Some(Bandwidth::from_kbps(y_kbps)),
                    duration: Some(Duration::from_millis(10)),
                }) as Box<dyn BwTraceConfig>,
            ])
            .build();

        let packet_queue = InfiniteQueue::<TestPacket<StdPacket>>::new(InfiniteQueueConfig {});
        let cell = BwReplayCell::new(
            Box::new(bandwidth_trace),
            packet_queue,
            BwType::NetworkLayer,
        )?;

        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        // All packets arrive at the real current instant, so the cell processes them in
        // real time no matter when the trace-start instant was calibrated (e.g. by another
        // test that ran earlier in the same process). The expected delays then depend on
        // the phase of the rectangular wave at the arrival instant, which is just the
        // arrival offset from the trace start.
        let trace_start = *TRACE_START_INSTANT
            .get()
            .expect("egress.reset() should initialize TRACE_START_INSTANT");
        let logical_send_time = Instant::now();
        let arrival_offset = logical_send_time - trace_start;
        let logical_start = Instant::now();

        for i in 0..PACKET_CNT {
            ingress.enqueue(TestPacket::with_timestamp(
                &[i as u8; BUFFER_SIZE],
                logical_send_time,
            ))?;
        }

        let expected_delays =
            expected_departure_delays(x_kbps, y_kbps, PACKET_CNT, L3_LEN, arrival_offset);

        let mut actual_receive_times = Vec::with_capacity(PACKET_CNT);
        let mut recv_cnt = 0;
        while recv_cnt < PACKET_CNT {
            let Some(received) = rt.block_on(async { egress.dequeue().await }) else {
                continue;
            };

            let expected_delay = expected_delays[recv_cnt];

            // Check 1: wall-clock receive time.
            let actual_receive_time = logical_start.elapsed();
            let expected_receive_time =
                (logical_send_time + expected_delay).saturating_duration_since(logical_start);
            let diff = actual_receive_time.as_secs_f64() - expected_receive_time.as_secs_f64();
            assert!(
                diff.abs() <= WALL_CLOCK_TOLERANCE.as_secs_f64(),
                "packet {recv_cnt}: expected to be received at {expected_receive_time:?}, got {actual_receive_time:?}"
            );
            actual_receive_times.push(actual_receive_time);

            // Check 2: logical timestamp when the packet leaves the cell.
            assert_eq!(
                expected_delay,
                received.delay(),
                "unexpected logical departure delay of packet {recv_cnt}"
            );
            assert_eq!(
                received.packet.buf[0] as usize, recv_cnt,
                "packets should leave the cell in FIFO order"
            );

            tracing::debug!(
                "Packet {recv_cnt} delayed by {:?} (logical) and {:?} (wall-clock)",
                received.delay(),
                actual_receive_time
            );
            recv_cnt += 1;
        }

        tracing::info!(?actual_receive_times, "Actual receive times:");
        tracing::info!(?expected_delays, "Expected receive times:");

        Ok(())
    }
}
