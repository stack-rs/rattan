//! A cell that replays packet delivery opportunities (PDOs), like the link queue of mahimahi.
//!
//! There is no concept of bandwidth here. A heavy PDO trace says when some bytes can leave the
//! cell. The packet at the head of the queue uses those bytes (counted by its L3 length, as
//! mahimahi does), so a large packet may need several PDOs and one PDO may send several small
//! packets. The bytes of a PDO that are not used when there is nothing to send are wasted.

use std::sync::Arc;

use async_trait::async_trait;
use netem_trace::model::HeavyPdoTraceConfig;
use netem_trace::HeavyPdoTrace;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::bandwidth::queue::{PacketQueue, AQM};
use super::{
    AtomicCellState, Cell, CellState, ControlInterface, Egress, Ingress, Packet, LARGE_DURATION,
    TRACE_START_INSTANT,
};
use crate::error::Error;
use crate::metal::timer::Timer;

/// The packet at the head of the queue, which is being sent over one or more PDOs.
struct Transmitting<P: Packet> {
    packet: P,
    bytes_left: u64,
}

impl<P: Packet> Transmitting<P> {
    fn new(packet: P) -> Self {
        Self {
            bytes_left: packet.l3_length() as u64,
            packet,
        }
    }
}

pub struct PdoReplayCellIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for PdoReplayCellIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for PdoReplayCellIngress<P>
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

pub struct PdoReplayCellEgress<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    egress: mpsc::UnboundedReceiver<P>,
    trace: Box<dyn HeavyPdoTrace>,
    packet_queue: AQM<Q, P>,
    /// The logical time of the last PDO read from the trace, or the start of the trace.
    last_pdo_time: Instant,
    /// The next PDO (logical time, bytes), read from the trace but not reached yet.
    next_pdo: Option<(Instant, u64)>,
    /// The PDO in use (logical time, bytes left).
    current_pdo: Option<(Instant, u64)>,
    transmitting: Option<Transmitting<P>>,
    config_rx: mpsc::UnboundedReceiver<PdoReplayCellConfig<P, Q>>,
    timer: Timer,
    state: AtomicCellState,
    notify_rx: Option<tokio::sync::broadcast::Receiver<crate::control::RattanNotify>>,
    started: bool,
}

impl<P, Q> PdoReplayCellEgress<P, Q>
where
    P: Packet + Send + Sync,
    Q: PacketQueue<P>,
{
    fn set_config(&mut self, config: PdoReplayCellConfig<P, Q>) {
        if let Some(trace_config) = config.trace_config {
            tracing::debug!("Set inner trace config");
            // The new trace starts now.
            self.trace = trace_config.into_model();
            self.last_pdo_time = Instant::now();
            self.next_pdo = None;
            self.current_pdo = None;
        }
        if let Some(queue_config) = config.queue_config {
            tracing::debug!(?queue_config, "Set inner queue config:");
            self.packet_queue.configure(queue_config);
        }
    }

    /// Returns the next PDO, reading it from the trace if needed. If the trace goes to end,
    /// there are no more PDOs, and the returned time is far in the future.
    fn peek_next_pdo(&mut self) -> (Instant, u64) {
        if let Some(pdo) = self.next_pdo {
            return pdo;
        }
        match self.trace.next_heavy_pdo() {
            Some((interval, bytes)) => {
                let pdo = (self.last_pdo_time + interval, bytes);
                self.last_pdo_time = pdo.0;
                self.next_pdo = Some(pdo);
                pdo
            }
            None => (self.last_pdo_time + LARGE_DURATION, 0),
        }
    }

    /// Skips the PDOs before the first packet that could use them, as their bytes would be
    /// wasted anyway. This avoids waking up for every PDO after the cell has been idle.
    fn skip_unused_pdos(&mut self) {
        let first_arrival = match self.transmitting.as_ref() {
            Some(transmitting) => transmitting.packet.get_timestamp(),
            // Only the inbound buffer of the AQM has packets.
            None if self.packet_queue.is_queue_empty() && !self.packet_queue.is_empty() => {
                self.packet_queue.next_call_time()
            }
            None => return,
        };
        while self.peek_next_pdo().0 < first_arrival && self.next_pdo.is_some() {
            self.next_pdo = None;
        }
    }

    /// Sends with the bytes left in the current PDO, and returns a packet once it is fully sent.
    /// The bytes left after that are kept for the next call, so that the next `dequeue()` can
    /// go on with the same PDO. When there is nothing to send at its time, the PDO is dropped.
    fn send_with_current_pdo(&mut self) -> Option<P> {
        let (time, mut bytes) = self.current_pdo.take()?;
        while bytes > 0 {
            if self.transmitting.is_none() {
                let packet = self.packet_queue.dequeue_at(time)?;
                self.transmitting = Some(Transmitting::new(packet));
            }
            let transmitting = self.transmitting.as_mut().unwrap();
            // A packet can not use a PDO before it arrives. This only happens with a zero-buffer
            // queue, where the packet does not wait in the AQM.
            if transmitting.packet.get_timestamp() > time {
                return None;
            }
            let sent = bytes.min(transmitting.bytes_left);
            transmitting.bytes_left -= sent;
            bytes -= sent;
            if transmitting.bytes_left == 0 {
                self.current_pdo = (bytes > 0).then_some((time, bytes));
                let mut packet = self.transmitting.take().unwrap().packet;
                packet.delay_until(time);
                return Some(packet);
            }
        }
        None
    }

    #[inline(always)]
    fn enqueue_packet(&mut self, new_packet: P) {
        if let Some(packet) = self.packet_queue.enqueue(new_packet) {
            // A zero-buffer queue returns the packet right away. It is sent if no other packet is
            // being sent, or dropped otherwise.
            if self.transmitting.is_none() {
                self.transmitting = Some(Transmitting::new(packet));
            }
        }
    }
}

#[async_trait]
impl<P, Q> Egress<P> for PdoReplayCellEgress<P, Q>
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

        loop {
            // The happy path: the current PDO still has bytes for the next packet.
            if let Some(packet) = self.send_with_current_pdo() {
                return Some(packet);
            }

            self.skip_unused_pdos();
            // Only wake up for a PDO if there is some packet to send.
            let has_packet = self.transmitting.is_some() || !self.packet_queue.is_empty();
            let (next_pdo_time, _) = self.peek_next_pdo();

            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.timer.sleep(next_pdo_time - Instant::now()), if has_packet => {
                    // Make sure that the packets that arrive by this PDO are in the AQM.
                    while let Ok(new_packet) = self.egress.try_recv() {
                        let new_packet = crate::check_cell_state!(self.state, new_packet);
                        self.enqueue_packet(new_packet);
                    }
                    self.current_pdo = self.next_pdo.take();
                }
                // `new_packet` can be None only if `self.egress` is closed.
                new_packet = self.egress.recv() => {
                    // XXX: If state change from `Normal` to others, the packets in the AQM may never be sent.
                    let new_packet = crate::check_cell_state!(self.state, new_packet?);
                    self.enqueue_packet(new_packet);
                }
            }
        }
    }

    // This must be called before any dequeue
    fn reset(&mut self) {
        self.last_pdo_time = *TRACE_START_INSTANT.get_or_init(Instant::now);
        self.next_pdo = None;
        self.current_pdo = None;
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
pub struct PdoReplayCellConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub trace_config: Option<Box<dyn HeavyPdoTraceConfig>>,
    pub queue_config: Option<Q::Config>,
}

impl<P, Q> Clone for PdoReplayCellConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
    Q::Config: Clone,
{
    fn clone(&self) -> Self {
        Self {
            trace_config: self.trace_config.clone(),
            queue_config: self.queue_config.clone(),
        }
    }
}

impl<P, Q> PdoReplayCellConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new<T: Into<Option<Box<dyn HeavyPdoTraceConfig>>>, U: Into<Option<Q::Config>>>(
        trace_config: T,
        queue_config: U,
    ) -> Self {
        Self {
            trace_config: trace_config.into(),
            queue_config: queue_config.into(),
        }
    }
}

pub struct PdoReplayCellControlInterface<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    config_tx: mpsc::UnboundedSender<PdoReplayCellConfig<P, Q>>,
}

impl<P, Q> ControlInterface for PdoReplayCellControlInterface<P, Q>
where
    P: Packet,
    Q: PacketQueue<P> + 'static,
{
    type Config = PdoReplayCellConfig<P, Q>;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        if config.trace_config.is_none() && config.queue_config.is_none() {
            // This ensures that incorrect HTTP requests will return errors.
            return Err(Error::ConfigError(
                "At least one of trace_config and queue_config should be set".to_string(),
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
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct PdoReplayCell<P: Packet, Q: PacketQueue<P>> {
    ingress: Arc<PdoReplayCellIngress<P>>,
    egress: PdoReplayCellEgress<P, Q>,
    control_interface: Arc<PdoReplayCellControlInterface<P, Q>>,
}

impl<P, Q> Cell<P> for PdoReplayCell<P, Q>
where
    P: Packet + Send + Sync + 'static,
    Q: PacketQueue<P> + 'static,
{
    type IngressType = PdoReplayCellIngress<P>;
    type EgressType = PdoReplayCellEgress<P, Q>;
    type ControlInterfaceType = PdoReplayCellControlInterface<P, Q>;

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

impl<P, Q> PdoReplayCell<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new(
        trace: Box<dyn HeavyPdoTrace>,
        packet_queue: Q,
    ) -> Result<PdoReplayCell<P, Q>, Error> {
        tracing::debug!("New PdoReplayCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(PdoReplayCell {
            ingress: Arc::new(PdoReplayCellIngress { ingress: rx }),
            egress: PdoReplayCellEgress {
                egress: tx,
                trace,
                packet_queue: AQM::new(packet_queue),
                last_pdo_time: Instant::now(),
                next_pdo: None,
                current_pdo: None,
                transmitting: None,
                config_rx,
                timer: Timer::new()?,
                state: AtomicCellState::new(CellState::Drop),
                notify_rx: None,
                started: false,
            },
            control_interface: Arc::new(PdoReplayCellControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::Duration;

    use netem_trace::model::{StaticHeavyPdoConfig, TraceHeavyPdoConfig};
    use netem_trace::{load_mahimahi_heavy_pdo_trace, MAHIMAHI_PDO_BYTES};
    use rstest::rstest;
    use tracing::{span, Level};

    use super::*;
    use crate::cells::bandwidth::queue::{
        DropTailQueue, DropTailQueueConfig, InfiniteQueue, InfiniteQueueConfig,
    };
    use crate::cells::bandwidth::BwType;
    use crate::cells::{StdPacket, TestPacket};

    type TP = TestPacket<StdPacket>;

    fn static_trace(interval_ms: u64, bytes: u64) -> Box<dyn HeavyPdoTrace> {
        Box::new(
            StaticHeavyPdoConfig::new()
                .interval(Duration::from_millis(interval_ms))
                .bytes(bytes)
                .count(0)
                .build(),
        )
    }

    /// Sends packets, each given as (arrival time in ms, L3 length), through a PdoReplayCell,
    /// and returns (packet index, logical departure time in ms) of the first `count` packets
    /// that leave the cell. All times are relative to the start of the trace.
    fn replay<Q: PacketQueue<TP> + 'static>(
        trace: Box<dyn HeavyPdoTrace>,
        packet_queue: Q,
        arrivals: &[(u64, usize)],
        count: usize,
    ) -> Result<Vec<(u8, u64)>, Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        let cell = PdoReplayCell::new(trace, packet_queue)?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        // `reset()` is not called, so the trace starts when the cell is created, instead of at
        // the shared TRACE_START_INSTANT, which may have been set long ago by another test.
        egress.change_state(CellState::Normal);
        let trace_start = egress.last_pdo_time;

        let arrivals = arrivals.to_vec();
        let _handle = rt.spawn(async move {
            for (i, (arrival_ms, l3_length)) in arrivals.into_iter().enumerate() {
                let arrival = trace_start + Duration::from_millis(arrival_ms);
                tokio::time::sleep_until(arrival).await;
                let mut buf = vec![0u8; l3_length + 14];
                buf[0] = i as u8;
                ingress.enqueue(TP::with_timestamp(&buf, arrival)).unwrap();
            }
            // Keep the ingress open, so that the egress keeps sending the queued packets.
            std::future::pending::<()>().await;
        });

        let mut departures = vec![];
        while departures.len() < count {
            // Fail instead of hanging forever if some packet never leaves the cell.
            let packet = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(5), egress.dequeue()).await
                })
                .expect("timed out waiting for a packet to leave the cell");
            let Some(packet) = packet else {
                continue;
            };
            let departure = packet.get_timestamp();
            // A packet never leaves before its logical departure time.
            assert!(departure <= Instant::now());
            departures.push((
                packet.as_slice()[0],
                (departure - trace_start).as_millis() as u64,
            ));
        }
        tracing::info!(?departures, "Departures:");
        Ok(departures)
    }

    #[test_log::test]
    fn pdo_bytes_carry_over() -> Result<(), Error> {
        let _span = span!(Level::INFO, "pdo_bytes_carry_over").entered();
        // Each PDO has 1500 bytes, each packet is 1000 bytes.
        // PDO at 10ms: packet 0 is sent, packet 1 gets 500 bytes.
        // PDO at 20ms: packet 1 gets the other 500 bytes and is sent, then packet 2 is sent.
        // PDO at 30ms: packet 3 is sent.
        let departures = replay(
            static_trace(10, 1500),
            InfiniteQueue::new(InfiniteQueueConfig {}).unwrap(),
            &[(5, 1000); 4],
            4,
        )?;
        assert_eq!(departures, vec![(0, 10), (1, 20), (2, 20), (3, 30)]);
        Ok(())
    }

    #[test_log::test]
    fn pdo_sends_many_small_packets() -> Result<(), Error> {
        let _span = span!(Level::INFO, "pdo_sends_many_small_packets").entered();
        // The 3 small packets go out with the PDO at 10ms, and the large packet gets the
        // remaining 1204 bytes. It needs the PDO at 20ms for the other 296 bytes.
        let departures = replay(
            static_trace(10, 1504),
            InfiniteQueue::new(InfiniteQueueConfig {}).unwrap(),
            &[(5, 100), (5, 100), (5, 100), (5, 1500)],
            4,
        )?;
        assert_eq!(departures, vec![(0, 10), (1, 10), (2, 10), (3, 20)]);
        Ok(())
    }

    #[test_log::test]
    fn unused_pdo_bytes_are_wasted() -> Result<(), Error> {
        let _span = span!(Level::INFO, "unused_pdo_bytes_are_wasted").entered();
        // The 500 bytes left in the PDO at 10ms and the PDOs from 20ms to 50ms are not used,
        // so packet 1, arriving at 52ms, waits for the PDO at 60ms.
        let departures = replay(
            static_trace(10, 1500),
            InfiniteQueue::new(InfiniteQueueConfig {}).unwrap(),
            &[(5, 1000), (52, 1000)],
            2,
        )?;
        assert_eq!(departures, vec![(0, 10), (1, 60)]);
        Ok(())
    }

    #[test_log::test]
    fn mahimahi_trace() -> Result<(), Error> {
        let _span = span!(Level::INFO, "mahimahi_trace").entered();
        // Two opportunities at 10ms (3008 bytes), then one at 30ms (1504 bytes).
        // Packets 0 and 1 are sent at 10ms, and packet 2 gets the 8 bytes left.
        // It gets the other 1492 bytes at 30ms.
        let trace = load_mahimahi_heavy_pdo_trace(vec![10, 10, 30], None, MAHIMAHI_PDO_BYTES)
            .unwrap()
            .build();
        let departures = replay(
            Box::new(trace),
            InfiniteQueue::new(InfiniteQueueConfig {}).unwrap(),
            &[(5, 1500); 3],
            3,
        )?;
        assert_eq!(departures, vec![(0, 10), (1, 10), (2, 30)]);
        Ok(())
    }

    #[test_log::test]
    fn zero_buffer_pdo() -> Result<(), Error> {
        let _span = span!(Level::INFO, "zero_buffer_pdo").entered();
        // Packet 1 arrives while packet 0 is waiting for the PDO at 10ms, so it is dropped.
        let departures = replay(
            static_trace(10, 1500),
            DropTailQueue::new(DropTailQueueConfig::new(None, 0, BwType::NetworkLayer)).unwrap(),
            &[(5, 1000), (7, 1000), (25, 1000)],
            2,
        )?;
        assert_eq!(departures, vec![(0, 10), (2, 30)]);
        Ok(())
    }

    /// Departures (packet index, ms) following `LinkQueue::use_a_delivery_opportunity` of
    /// mahimahi, given the PDOs as (time in ms, bytes) and the arrivals as (time in ms, L3 length).
    fn mahimahi_departures(pdos: &[(u64, u64)], arrivals: &[(u64, usize)]) -> Vec<(u8, u64)> {
        let mut queue = VecDeque::new();
        let mut next_arrival = 0;
        let mut in_transit: Option<(usize, u64)> = None;
        let mut departures = vec![];
        for &(time, mut bytes) in pdos {
            while next_arrival < arrivals.len() && arrivals[next_arrival].0 <= time {
                queue.push_back(next_arrival);
                next_arrival += 1;
            }
            while bytes > 0 {
                if in_transit.is_none() {
                    let Some(i) = queue.pop_front() else {
                        break;
                    };
                    in_transit = Some((i, arrivals[i].1 as u64));
                }
                let (i, bytes_left) = in_transit.as_mut().unwrap();
                let sent = bytes.min(*bytes_left);
                *bytes_left -= sent;
                bytes -= sent;
                if *bytes_left == 0 {
                    departures.push((*i as u8, time));
                    in_transit = None;
                }
            }
        }
        departures
    }

    #[rstest]
    #[case(1)]
    #[case(2)]
    #[case(3)]
    #[test_log::test]
    fn random_trace_matches_mahimahi(#[case] seed: u64) -> Result<(), Error> {
        let _span = span!(Level::INFO, "random_trace_matches_mahimahi").entered();
        // A small LCG, so that each case is reproducible.
        let mut state = seed;
        let mut random = |n: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % n
        };

        // PDOs are on multiples of 10ms and packets arrive 5ms after a multiple of 10ms, so
        // that the order of a PDO and an arrival never depends on how the timers wake up.
        let mut pattern = vec![];
        let mut pdos = vec![];
        let mut time = 0;
        for _ in 0..40 {
            let interval = random(3) * 10;
            let bytes = [100, 1400, 1504, 3008][random(4) as usize];
            time += interval;
            pattern.push((Duration::from_millis(interval), bytes));
            pdos.push((time, bytes));
        }
        let mut arrivals: Vec<(u64, usize)> = (0..30)
            .map(|_| (random(time / 10) * 10 + 5, 40 + random(1461) as usize))
            .collect();
        arrivals.sort();

        let expected = mahimahi_departures(&pdos, &arrivals);
        let trace = TraceHeavyPdoConfig::new().pattern(pattern).build();
        let departures = replay(
            Box::new(trace),
            InfiniteQueue::new(InfiniteQueueConfig {}).unwrap(),
            &arrivals,
            expected.len(),
        )?;
        assert_eq!(departures, expected);
        Ok(())
    }
}
