use std::{fmt::Debug, sync::Arc};

use async_trait::async_trait;
use netem_trace::{model::DelayTraceConfig, Delay, DelayTrace};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::{sync::mpsc, time::Instant};

use super::{TimedConfig, LARGE_DURATION, TRACE_START_INSTANT};
#[cfg(test)]
use crate::cells::relative_time;
use crate::cells::{AtomicCellState, Cell, CellState, ControlInterface, Egress, Ingress, Packet};
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
    delay: Delay,
    config_rx: mpsc::UnboundedReceiver<DelayCellConfig>,
    timer: Timer,
    state: AtomicCellState,
    notify_rx: Option<tokio::sync::broadcast::Receiver<crate::control::RattanNotify>>,
    started: bool,
    latest_egress_timestamp: Instant,
}

impl<P> DelayCellEgress<P>
where
    P: Packet + Send + Sync,
{
    fn set_config(&mut self, config: DelayCellConfig) {
        self.delay = config.delay;
    }
}

#[async_trait]
impl<P> Egress<P> for DelayCellEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // Wait for Start notify if not started yet
        crate::wait_until_started!(self, Start);

        let mut packet = loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                // `packet` can be None only if `self.egress` is closed.
                packet = self.egress.recv() => {
                    break crate::check_cell_state!(self.state, packet?);
                }
            }
        };

        // Logical timestamps are considered non-decreasing.
        let timestamp = packet.get_timestamp();

        let send_time = loop {
            let logical_send_time = (timestamp + self.delay).max(self.latest_egress_timestamp);

            let sleep_time = logical_send_time.duration_since(Instant::now());

            if sleep_time.is_zero() {
                break logical_send_time;
            }

            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.timer.sleep_until(logical_send_time) => {
                    break logical_send_time;
                }
            }
        };

        self.latest_egress_timestamp = send_time;

        packet.delay_until(send_time);
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
        tracing::info!("Setting delay to {:?}", config.delay);
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
        tracing::debug!(?delay, "New DelayCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();

        let logical_time = *CALIBRATED_START_INSTANT.get_or_init(Instant::now);
        Ok(DelayCell {
            ingress: Arc::new(DelayCellIngress { ingress: rx }),
            egress: DelayCellEgress {
                egress: tx,
                delay,
                config_rx,
                timer: Timer::new()?,
                state: AtomicCellState::new(CellState::Drop),
                notify_rx: None,
                started: false,
                latest_egress_timestamp: logical_time,
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
    trace: Box<dyn DelayTrace>,
    delays: TimedConfig<Delay>,
    next_change: Instant,
    config_rx: mpsc::UnboundedReceiver<DelayReplayCellConfig>,
    send_timer: Timer,
    change_timer: Timer,
    state: AtomicCellState,
    notify_rx: Option<tokio::sync::broadcast::Receiver<crate::control::RattanNotify>>,
    started: bool,
    latest_egress_timestamp: Instant,
}

impl<P> DelayReplayCellEgress<P>
where
    P: Packet + Send + Sync,
{
    fn set_config(&mut self, config: DelayReplayCellConfig) {
        tracing::debug!("Set inner trace config");
        self.trace = config.trace_config.into_model();
    }

    /// If the trace does not go to end and a new config was set, returns true and update self.next_change.
    /// If the trace goes to end, returns false, returns false and disable self.next_change. In such case,
    /// the config is not updated so that the latest value is used.
    fn update_delay(&mut self, timestamp: Instant) -> bool {
        if let Some((current_delay, duration)) = self.trace.next_delay() {
            #[cfg(test)]
            tracing::trace!(
                "Setting {:?} delay valid from {:?} utill {:?}",
                current_delay,
                relative_time(timestamp),
                relative_time(timestamp + duration),
            );
            self.delays.update(current_delay, timestamp);
            self.next_change = timestamp + duration;
            true
        } else {
            tracing::debug!("Trace goes to end in DelayReplay Cell");
            self.next_change = timestamp + LARGE_DURATION;
            false
        }
    }
}

#[async_trait]
impl<P> Egress<P> for DelayReplayCellEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // Wait for FirstPacket notify if not started yet
        #[cfg(feature = "first-packet")]
        crate::wait_until_started!(self, FirstPacket);
        #[cfg(not(feature = "first-packet"))]
        crate::wait_until_started!(self, Start);

        let mut packet = loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                // `packet` can be None only if `self.egress` is closed.
                packet = self.egress.recv() => {
                    break crate::check_cell_state!(self.state, packet?);
                }
            }
        };

        let timestamp = packet.get_timestamp();

        // Update the config until it is applicable for the packet.
        while self.next_change <= timestamp {
            self.update_delay(self.next_change);
        }

        let send_time = loop {
            // `get_current` returns None if and only if `self.delays` has never been updated
            // since last reset.
            let logical_send_time =
                if let Some(delay_time) = self.delays.get_at_timestamp(timestamp) {
                    (timestamp + *delay_time).max(self.latest_egress_timestamp)
                } else {
                    timestamp.max(self.latest_egress_timestamp)
                };
            let sleep_time = logical_send_time.duration_since(Instant::now());

            if sleep_time.is_zero() {
                // Send immediately.
                break logical_send_time;
            }

            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.change_timer.sleep_until(self.next_change), if self.next_change <= logical_send_time => {
                    self.update_delay(self.next_change);
                }
                _ = self.send_timer.sleep(sleep_time), if self.next_change > logical_send_time => {
                    break logical_send_time;
                }
            }
        };

        self.latest_egress_timestamp = send_time;

        packet.delay_until(send_time);
        Some(packet)
    }

    // This must be called before any dequeue
    fn reset(&mut self) {
        self.next_change = *TRACE_START_INSTANT.get_or_init(Instant::now);
        tracing::debug!(
            "calculate next delay for logical trace change time {:?} (reset)",
            self.next_change
        );
        self.delays.reset();
        self.update_delay(self.next_change);
        self.latest_egress_timestamp = *CALIBRATED_START_INSTANT.get_or_init(Instant::now);
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
        tracing::info!("Setting delay replay config");
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
                trace,
                delays: TimedConfig::default(),
                next_change: Instant::now(),
                config_rx,
                send_timer: Timer::new()?,
                change_timer: Timer::new()?,
                state: AtomicCellState::new(CellState::Drop),
                latest_egress_timestamp: *CALIBRATED_START_INSTANT.get_or_init(Instant::now),
                notify_rx: None,
                started: false,
            },
            control_interface: Arc::new(DelayReplayCellControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use itertools::Itertools;
    use netem_trace::model::{RepeatedDelayPatternConfig, StaticDelayConfig};
    use std::time::Duration;
    use tokio::time::Instant;
    use tracing::{info, span, Level};

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
            egress.change_state(CellState::Normal);

            info!("Testing delay time for {}ms delay cell", testing_delay);
            let mut delays: Vec<f64> = Vec::new();

            for _ in 0..10 {
                let start = Instant::now();
                let test_packet = TestPacket::<StdPacket>::with_timestamp(&[0; 256], start);
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
            tracing::debug!("Delays: {:?}", delays);
            info!(
                "Average delay: {:.3}ms, error {:.1}ms",
                average_delay,
                (average_delay - testing_delay as f64)
            );
            // Check the delay time
            assert!((average_delay - testing_delay as f64).abs() <= DELAY_ACCURACY_TOLERANCE);
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
        egress.change_state(CellState::Normal);

        //Test whether the packet will wait longer if the config is updated
        let start = Instant::now();
        let test_packet = TestPacket::<StdPacket>::with_timestamp(&[0; 256], start);

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

        let start = Instant::now();
        let test_packet = TestPacket::<StdPacket>::with_timestamp(&[0; 256], start);

        ingress.enqueue(test_packet)?;

        // Wait for 15ms, then change the config back to 10ms
        std::thread::sleep(Duration::from_millis(15));
        config_changer.set_config(DelayCellConfig::new(Duration::from_millis(10)))?;

        // The expected behaviour is that the packet exits the cell almost immediately.
        let received = rt.block_on(async { egress.dequeue().await });

        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay after update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);
        assert_eq!(received.delay(), Duration::from_millis(10));
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
        egress.change_state(CellState::Normal);
        let start_time = tokio::time::Instant::now();
        let mut delays: Vec<f64> = Vec::new();
        let mut real_delays: Vec<Duration> = Vec::new();
        for interval in [1100, 2100, 3100, 0] {
            for _ in 0..10 {
                let start = Instant::now();
                let test_packet = TestPacket::<StdPacket>::with_timestamp(&[0; 256], start);

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
        assert_eq!(real_delays.len(), 40);

        for (idx, calibrated_delay) in vec![10, 50, 10, 50].into_iter().enumerate() {
            tracing::debug!("Expected delay {}ms", calibrated_delay);
            let range = (idx * 10)..(10 + idx * 10);

            let average_delay = delays[range.clone()].iter().sum::<f64>() / 10.0;
            tracing::debug!("Delays: {:?}", delays[range.clone()].iter().collect_vec());
            tracing::debug!(
                "Real Delays: {:?}",
                real_delays[range.clone()].iter().collect_vec()
            );
            info!(
                "Average delay: {:.3}ms, error {:.1}ms",
                average_delay,
                (average_delay - calibrated_delay as f64)
            );
            // Check the delay time
            assert!((average_delay - calibrated_delay as f64).abs() <= DELAY_ACCURACY_TOLERANCE);

            for delay in real_delays[range.clone()].iter() {
                assert_eq!(delay, &Duration::from_millis(calibrated_delay));
            }
        }
        Ok(())
    }

    #[test_log::test]
    fn test_replay_delay_cell_change_state() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_replay_delay_cell_change_state").entered();
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
            let test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256]);
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });
            // Should drop all packets
            assert!(received.is_none());
        }
        egress.change_state(CellState::PassThrough);
        let mut delays: Vec<f64> = Vec::new();
        let mut real_delays: Vec<_> = Vec::new();
        rt.block_on(async {
            tokio::time::sleep_until(start_time + Duration::from_millis(1100)).await;
        });
        info!("Start pass through {:?}", relative_time(Instant::now()));
        for _ in 0..10 {
            let test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256]);
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
        egress.change_state(CellState::Normal);
        for interval in [2100, 3100] {
            rt.block_on(async {
                tokio::time::sleep_until(start_time + Duration::from_millis(interval)).await;
            });

            info!("Begin test at {:?}", relative_time(Instant::now()));

            for _ in 0..10 {
                let test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256]);
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
            tracing::debug!("Delays: {:?}", delays);
            info!(
                "Average delay: {:.3}ms, error {:.1}ms",
                average_delay,
                (average_delay - calibrated_delay as f64)
            );
            // Check the delay time
            assert!((average_delay - calibrated_delay as f64).abs() <= DELAY_ACCURACY_TOLERANCE);

            for delay in real_delays[(idx * 10)..(10 + idx * 10)].iter() {
                assert_eq!(delay, &Duration::from_millis(calibrated_delay));
            }
        }
        Ok(())
    }
}
