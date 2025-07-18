use std::{fmt::Debug, sync::Arc};

use async_trait::async_trait;
use netem_trace::{model::DelayTraceConfig, Delay, DelayTrace};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::{sync::mpsc, time::Instant};
use tracing::{debug, info};

use crate::cells::bandwidth::LARGE_DURATION;
use crate::cells::{
    per_packet::DelayedQueue, AtomicState, Cell, Configs, ControlInterface, Egress, Ingress,
    Packet, State,
};
use crate::core::CALIBRATED_START_INSTANT;
use crate::error::Error;
use crate::metal::timer::Timer;

pub struct DelayCellIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for DelayCellIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for DelayCellIngress<P>
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

pub struct DelayCellEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<P>,
    delays: Configs<Delay>,
    queue: DelayedQueue<P>,
    config_rx: mpsc::UnboundedReceiver<DelayCellConfig>,
    timer: Timer,
    state: AtomicState,
}

impl<P> DelayCellEgress<P>
where
    P: Packet + Send + Sync,
{
    fn set_config(&mut self, config: DelayCellConfig) {
        debug!(
            before = ?self.delays.last_key_value(),
            after = ?config.delay,
            "Set inner delay:"
        );
        self.delays.insert(Instant::now(), config.delay);
    }
}

impl Configs<Delay> {
    fn delay(&self, packet_timestamp: Instant) -> Delay {
        let mut packet_delay = Delay::ZERO;

        for (timestamp, delay) in self.iter() {
            if *timestamp <= packet_timestamp {
                packet_delay = *delay;
            } else if packet_timestamp + packet_delay < *timestamp {
                // If the delay applies after the packet exited the queue
                // then this new delay does not apply and all after neither
                break;
            } else if packet_timestamp + *delay <= *timestamp {
                // If the delay applies before the packet exited the queue
                // but with the new delay it should have left before the new delay
                // then the packet leaves immediately and no further new delay apply
                packet_delay = *timestamp - packet_timestamp;
                break;
            } else {
                packet_delay = *delay
            }
        }
        packet_delay
    }
}

#[async_trait]
impl<P> Egress<P> for DelayCellEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // Wait for the next packet to arrive
        loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                packet = self.egress.recv() => {
                    match packet {
                        Some(packet) => match self.state.load(std::sync::atomic::Ordering::Acquire) {
                            State::Drop => {
                                return None;
                            }
                            State::PassThrough => {
                                return Some(packet);
                            }
                            State::Normal => {
                                self.queue.enqueue(packet, Delay::ZERO)
                            }
                        }
                        // Channel closed
                        None => return None,
                    }
                }
                _ = self.timer.sleep_until({
                    let timestamp = self.queue.next_packet().map(|packet| packet.get_timestamp()).unwrap_or_else(|| Instant::now());
                    timestamp + self.delays.delay(timestamp)
                }) => {
                    if !self.queue.is_empty() {
                        break
                    }
                }
            }
        }
        debug_assert!(self.queue.next_instant() <= Instant::now());
        debug_assert!(!self.queue.is_empty());
        let (timestamp, mut packet) = self.queue.dequeue().expect("There should be something");
        packet.delay_by(self.delays.delay(timestamp));
        Some(packet)
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state.into(), std::sync::atomic::Ordering::Release);
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Default, Clone)]
pub struct DelayCellConfig {
    #[cfg_attr(feature = "serde", serde(with = "crate::utils::serde::duration"))]
    pub delay: Delay,
}

impl DelayCellConfig {
    pub fn new<T: Into<Delay>>(delay: T) -> Self {
        Self {
            delay: delay.into(),
        }
    }
}

pub struct DelayCellControlInterface {
    config_tx: mpsc::UnboundedSender<DelayCellConfig>,
}

impl ControlInterface for DelayCellControlInterface {
    type Config = DelayCellConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Setting delay to {:?}", config.delay);
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct DelayCell<P: Packet> {
    ingress: Arc<DelayCellIngress<P>>,
    egress: DelayCellEgress<P>,
    control_interface: Arc<DelayCellControlInterface>,
}

impl<P> Cell<P> for DelayCell<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = DelayCellIngress<P>;
    type EgressType = DelayCellEgress<P>;
    type ControlInterfaceType = DelayCellControlInterface;

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

impl<P> DelayCell<P>
where
    P: Packet,
{
    pub fn new<D: Into<Option<Delay>>>(delay: D) -> Result<DelayCell<P>, Error> {
        let delay = delay.into().unwrap_or_default();
        debug!(?delay, "New DelayCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(DelayCell {
            ingress: Arc::new(DelayCellIngress { ingress: rx }),
            egress: DelayCellEgress {
                egress: tx,
                delays: Configs::from([(Instant::now(), delay)]),
                queue: DelayedQueue::new(),
                config_rx,
                timer: Timer::new()?,
                state: AtomicState::new(State::Drop),
            },
            control_interface: Arc::new(DelayCellControlInterface { config_tx }),
        })
    }
}

type DelayReplayCellIngress<P> = DelayCellIngress<P>;

pub struct DelayReplayCellEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<P>,
    current_trace: Box<dyn DelayTrace>,
    next_traces: Configs<Box<dyn DelayTrace>>,
    delays: Configs<Option<Delay>>,
    queue: DelayedQueue<P>,
    next_change: Instant,
    config_rx: mpsc::UnboundedReceiver<DelayReplayCellConfig>,
    send_timer: Timer,
    change_timer: Timer,
    state: AtomicState,
}

impl<P> DelayReplayCellEgress<P>
where
    P: Packet + Send + Sync,
{
    fn set_config(&mut self, config: DelayReplayCellConfig) {
        tracing::debug!("Set inner trace config");
        let trace = config.trace_config.into_model();
        self.next_traces.insert(Instant::now(), trace);

        // if self.next_change(now).is_none() {
        //     // handle null trace outside this function
        //     tracing::warn!("Setting null trace");
        //     self.next_change = now;
        //     // set state to 0 to indicate the trace goes to end and the cell will drop all packets
        //     self.change_state(0);
        // }
    }

    fn update_trace(&mut self) {
        if let Some((timestamp, trace)) = self.next_traces.next_config(self.next_change) {
            self.current_trace = trace;
            self.next_delay(timestamp);
        }
    }

    fn next_delay(&mut self, timestamp: Instant) {
        if let Some((delay, duration)) = self.current_trace.next_delay() {
            self.delays.insert(timestamp, Some(delay));
            self.next_change = timestamp + duration;
        } else {
            debug!("Trace goes to end");
            self.delays.insert(timestamp, None);
            self.next_change = timestamp + LARGE_DURATION
        }
    }
}

impl Configs<Option<Delay>> {
    fn delay(&self, packet_timestamp: Instant) -> Option<Delay> {
        let mut packet_delay = None;
        for (timestamp, delay) in self.iter() {
            if *timestamp <= packet_timestamp {
                packet_delay = *delay;
            } else if let Some(ref mut packet_delay) = packet_delay {
                if packet_timestamp + *packet_delay < *timestamp {
                    // If the delay applies after the packet exited the queue
                    // then this new delay does not apply and all after neither
                    break;
                } else if let Some(delay) = delay {
                    if packet_timestamp + *delay <= *timestamp {
                        // If the delay applies before the packet exited the queue
                        // but with the new delay it should have left before the new delay
                        // then the packet leaves immediately and no further new delay apply
                        *packet_delay = *timestamp - packet_timestamp;
                        break;
                    } else {
                        *packet_delay = *delay
                    }
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
        packet_delay
    }
}

#[async_trait]
impl<P> Egress<P> for DelayReplayCellEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                packet = self.egress.recv() => {
                    match packet {
                        Some(packet) => match self.state.load(std::sync::atomic::Ordering::Acquire) {
                            State::Drop => {
                                return None;
                            }
                            State::PassThrough => {
                                return Some(packet);
                            }
                            State::Normal => {
                                self.queue.enqueue(packet, Delay::ZERO)
                            }
                        }
                        // Channel closed
                        None => return None,
                    }
                }
                _ = self.send_timer.sleep_until({
                    let timestamp = self.queue.next_packet().map(|packet| packet.get_timestamp()).unwrap_or_else(|| Instant::now());
                    if let Some(delay) = self.delays.delay(timestamp) {
                        timestamp + delay
                    } else {
                        timestamp
                    }
                }) => {
                    if !self.queue.is_empty() {
                        break
                    }
                }
                _ = self.change_timer.sleep_until(self.next_change) => {
                    self.next_delay(self.next_change);
                }

            }
        }

        debug_assert!(self.queue.next_instant() <= Instant::now());
        debug_assert!(!self.queue.is_empty());
        let (timestamp, mut packet) = self.queue.dequeue().expect("There should be something");
        if let Some(delay) = self.delays.delay(timestamp) {
            packet.delay_by(delay);
            Some(packet)
        } else {
            None
        }
    }

    // This must be called before any dequeue
    fn reset(&mut self) {
        // self.next_available = *CALIBRATED_START_INSTANT.get_or_init(Instant::now);
        // self.next_change = *CALIBRATED_START_INSTANT.get_or_init(Instant::now);
        todo!()
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state.into(), std::sync::atomic::Ordering::Release);
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DelayReplayCellConfig {
    pub trace_config: Box<dyn DelayTraceConfig>,
}

impl Clone for DelayReplayCellConfig {
    fn clone(&self) -> Self {
        Self {
            trace_config: self.trace_config.clone(),
        }
    }
}

impl DelayReplayCellConfig {
    pub fn new<T: Into<Box<dyn DelayTraceConfig>>>(trace_config: T) -> Self {
        Self {
            trace_config: trace_config.into(),
        }
    }
}

pub struct DelayReplayCellControlInterface {
    config_tx: mpsc::UnboundedSender<DelayReplayCellConfig>,
}

impl ControlInterface for DelayReplayCellControlInterface {
    type Config = DelayReplayCellConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Setting delay replay config");
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct DelayReplayCell<P: Packet> {
    ingress: Arc<DelayReplayCellIngress<P>>,
    egress: DelayReplayCellEgress<P>,
    control_interface: Arc<DelayReplayCellControlInterface>,
}

impl<P> Cell<P> for DelayReplayCell<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = DelayReplayCellIngress<P>;
    type EgressType = DelayReplayCellEgress<P>;
    type ControlInterfaceType = DelayReplayCellControlInterface;

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

impl<P> DelayReplayCell<P>
where
    P: Packet,
{
    pub fn new(trace: Box<dyn DelayTrace>) -> Result<DelayReplayCell<P>, Error> {
        tracing::debug!("New DelayReplayCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(DelayReplayCell {
            ingress: Arc::new(DelayReplayCellIngress { ingress: rx }),
            egress: DelayReplayCellEgress {
                egress: tx,
                current_trace: trace,
                next_traces: Configs::new(),
                delays: Configs::new(),
                next_change: Instant::now(),
                queue: DelayedQueue::new(),
                config_rx,
                send_timer: Timer::new()?,
                change_timer: Timer::new()?,
                state: AtomicState::new(State::Drop),
            },
            control_interface: Arc::new(DelayReplayCellControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use netem_trace::model::{RepeatedDelayPatternConfig, StaticDelayConfig};
    use std::time::Duration;
    use tokio::time::Instant;
    use tracing::{span, Level};

    use crate::cells::{StdPacket, TestPacket};

    use super::*;

    // The tolerance of the accuracy of the delays, in ms
    const DELAY_ACCURACY_TOLERANCE: f64 = 1.0;
    // List of delay times to be tested
    const DELAY_TEST_TIME: [u64; 8] = [0, 2, 5, 10, 20, 50, 100, 500];

    #[test_log::test]
    fn test_delay_cell() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_delay_cell").entered();
        for testing_delay in DELAY_TEST_TIME {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            let _guard = rt.enter();

            info!("Creating cell with {}ms delay", testing_delay);
            let cell = DelayCell::new(Duration::from_millis(testing_delay))?;
            let ingress = cell.sender();
            let mut egress = cell.into_receiver();
            egress.reset();
            egress.change_state(2);

            info!("Testing delay time for {}ms delay cell", testing_delay);
            let mut delays: Vec<f64> = Vec::new();

            for _ in 0..10 {
                let test_packet =
                    TestPacket::<StdPacket>::from_raw_buffer(&[0; 256], Instant::now());
                let start = Instant::now();
                ingress.enqueue(test_packet)?;
                let received = rt.block_on(async { egress.dequeue().await });

                // Use microsecond to get precision up to 0.001ms
                let duration = start.elapsed().as_micros() as f64 / 1000.0;

                delays.push(duration);

                // Should never loss packet
                assert!(received.is_some());

                let received = received.unwrap();

                // The length should be correct
                assert!(received.length() == 256);

                assert_eq!(received.delay(), Duration::from_millis(testing_delay));
            }

            info!(
                "Tested delays for {}ms delay cell: {:?}",
                testing_delay, delays
            );

            let average_delay = delays.iter().sum::<f64>() / 10.0;
            debug!("Delays: {:?}", delays);
            info!(
                "Average delay: {:.3}ms, error {:.1}ms",
                average_delay,
                (average_delay - testing_delay as f64).abs()
            );
            // Check the delay time
            assert!((average_delay - testing_delay as f64) <= DELAY_ACCURACY_TOLERANCE);
        }

        Ok(())
    }

    #[test_log::test]
    fn test_delay_cell_config_update() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_delay_cell_config_update").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let _guard = rt.enter();

        info!("Creating cell with 10ms delay");
        let cell = DelayCell::new(Duration::from_millis(10))?;
        let config_changer = cell.control_interface();
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        //Test whether the packet will wait longer if the config is updated
        let test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256], Instant::now());

        let start = Instant::now();
        ingress.enqueue(test_packet)?;

        // Wait for 5ms, then change the config to let the delay be longer
        std::thread::sleep(Duration::from_millis(5));
        config_changer.set_config(DelayCellConfig::new(Duration::from_millis(20)))?;

        let received = rt.block_on(async { egress.dequeue().await });

        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay after update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);
        assert_eq!(received.delay(), Duration::from_millis(20));

        assert!((duration - 20.0).abs() <= DELAY_ACCURACY_TOLERANCE);

        // Test whether the packet will be returned immediately when the new delay is less than the already passed time
        let test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256], Instant::now());

        let start = Instant::now();
        ingress.enqueue(test_packet)?;

        // Wait for 15ms, then change the config back to 10ms
        std::thread::sleep(Duration::from_millis(15));
        config_changer.set_config(DelayCellConfig::new(Duration::from_millis(10)))?;

        let received = rt.block_on(async { egress.dequeue().await });

        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay after update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);
        assert_eq!(received.delay(), Duration::from_millis(15));

        assert!((duration - 15.0).abs() <= DELAY_ACCURACY_TOLERANCE);

        Ok(())
    }

    #[test_log::test]
    fn test_replay_delay_cell() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_replay_delay_cell").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let _guard = rt.enter();

        let pattern = vec![
            Box::new(
                StaticDelayConfig::new()
                    .delay(Delay::from_millis(10))
                    .duration(Duration::from_secs(1)),
            ) as Box<dyn DelayTraceConfig>,
            Box::new(
                StaticDelayConfig::new()
                    .delay(Delay::from_millis(50))
                    .duration(Duration::from_secs(1)),
            ) as Box<dyn DelayTraceConfig>,
        ];
        let delay_trace_config =
            Box::new(RepeatedDelayPatternConfig::new().pattern(pattern).count(0))
                as Box<dyn DelayTraceConfig>;
        let delay_trace = delay_trace_config.into_model();
        let cell = DelayReplayCell::new(delay_trace)?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);
        let start_time = tokio::time::Instant::now();
        let mut delays: Vec<f64> = Vec::new();
        let mut real_delays: Vec<Duration> = Vec::new();
        for interval in [1100, 2100, 3100, 0] {
            for _ in 0..10 {
                let test_packet =
                    TestPacket::<StdPacket>::from_raw_buffer(&[0; 256], Instant::now());
                let start = Instant::now();

                ingress.enqueue(test_packet)?;
                let received = rt.block_on(async { egress.dequeue().await });

                // Use microsecond to get precision up to 0.001ms
                let duration = start.elapsed().as_micros() as f64 / 1000.0;

                delays.push(duration);

                // Should never loss packet
                assert!(received.is_some());

                let received = received.unwrap();

                // The length should be correct
                assert!(received.length() == 256);

                real_delays.push(received.delay());
            }
            rt.block_on(async {
                tokio::time::sleep_until(start_time + Duration::from_millis(interval)).await;
            })
        }
        assert_eq!(delays.len(), 40);
        for (idx, calibrated_delay) in vec![10, 50, 10, 50].into_iter().enumerate() {
            let average_delay = delays[(idx * 10)..(10 + idx * 10)].iter().sum::<f64>() / 10.0;
            debug!("Delays: {:?}", delays);
            info!(
                "Average delay: {:.3}ms, error {:.1}ms",
                average_delay,
                (average_delay - calibrated_delay as f64).abs()
            );
            // Check the delay time
            assert!((average_delay - calibrated_delay as f64) <= DELAY_ACCURACY_TOLERANCE);

            for delay in real_delays[(idx * 10)..(10 + idx * 10)].iter() {
                assert_eq!(delay, &Duration::from_millis(calibrated_delay));
            }
        }
        Ok(())
    }

    #[test_log::test]
    fn test_replay_delay_cell_change_state() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_replay_delay_cell").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let _guard = rt.enter();

        let pattern = vec![
            Box::new(
                StaticDelayConfig::new()
                    .delay(Delay::from_millis(10))
                    .duration(Duration::from_secs(1)),
            ) as Box<dyn DelayTraceConfig>,
            Box::new(
                StaticDelayConfig::new()
                    .delay(Delay::from_millis(50))
                    .duration(Duration::from_secs(1)),
            ) as Box<dyn DelayTraceConfig>,
        ];
        let delay_trace_config =
            Box::new(RepeatedDelayPatternConfig::new().pattern(pattern).count(0))
                as Box<dyn DelayTraceConfig>;
        let delay_trace = delay_trace_config.into_model();
        let cell = DelayReplayCell::new(delay_trace)?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        let start_time = tokio::time::Instant::now();
        for _ in 0..10 {
            let test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256], Instant::now());
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });
            // Should drop all packets
            assert!(received.is_none());
        }
        egress.change_state(1);
        let mut delays: Vec<f64> = Vec::new();
        let mut real_delays: Vec<_> = Vec::new();
        rt.block_on(async {
            tokio::time::sleep_until(start_time + Duration::from_millis(1100)).await;
        });
        for _ in 0..10 {
            let test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256], Instant::now());
            let start = Instant::now();
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });

            // Use microsecond to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == 256);

            real_delays.push(received.delay());
        }
        for interval in [2100, 3100] {
            rt.block_on(async {
                tokio::time::sleep_until(start_time + Duration::from_millis(interval)).await;
            });
            for _ in 0..10 {
                let test_packet =
                    TestPacket::<StdPacket>::from_raw_buffer(&[0; 256], Instant::now());
                let start = Instant::now();
                ingress.enqueue(test_packet)?;
                let received = rt.block_on(async { egress.dequeue().await });

                // Use microsecond to get precision up to 0.001ms
                let duration = start.elapsed().as_micros() as f64 / 1000.0;

                delays.push(duration);

                // Should never loss packet
                assert!(received.is_some());

                let received = received.unwrap();

                // The length should be correct
                assert!(received.length() == 256);

                real_delays.push(received.delay());
            }
        }
        assert_eq!(delays.len(), 30);
        for (idx, calibrated_delay) in vec![0, 10, 50].into_iter().enumerate() {
            let average_delay = delays[(idx * 10)..(10 + idx * 10)].iter().sum::<f64>() / 10.0;
            debug!("Delays: {:?}", delays);
            info!(
                "Average delay: {:.3}ms, error {:.1}ms",
                average_delay,
                (average_delay - calibrated_delay as f64).abs()
            );
            // Check the delay time
            assert!((average_delay - calibrated_delay as f64) <= DELAY_ACCURACY_TOLERANCE);

            for delay in real_delays[(idx * 10)..(10 + idx * 10)].iter() {
                assert_eq!(delay, &Duration::from_millis(calibrated_delay));
            }
        }
        Ok(())
    }
}
