use crate::devices::{Device, Packet};
use crate::error::Error;
use crate::metal::timer::Timer;
use async_trait::async_trait;
use netem_trace::model::DelayTraceConfig;
use netem_trace::{Delay, DelayTrace};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info};

use super::{ControlInterface, Egress, Ingress};

pub struct DelayDeviceIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for DelayDeviceIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for DelayDeviceIngress<P>
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

pub struct DelayDeviceEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<P>,
    delay: Delay,
    config_rx: mpsc::UnboundedReceiver<DelayDeviceConfig>,
    timer: Timer,
}

impl<P> DelayDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    fn set_config(&mut self, config: DelayDeviceConfig) {
        debug!(
            before = ?self.delay,
            after = ?config.delay,
            "Set inner delay:"
        );
        self.delay = config.delay;
    }
}

#[async_trait]
impl<P> Egress<P> for DelayDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let packet = match self.egress.recv().await {
            Some(packet) => packet,
            None => return None,
        };
        loop {
            tokio::select! {
                biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                _ = self.timer.sleep(packet.get_timestamp() + self.delay - Instant::now()) => {
                    break;
                }
            }
        }
        Some(packet)
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Default, Clone)]
pub struct DelayDeviceConfig {
    #[cfg_attr(feature = "serde", serde(with = "humantime_serde"))]
    pub delay: Delay,
}

impl DelayDeviceConfig {
    pub fn new<T: Into<Delay>>(delay: T) -> Self {
        Self {
            delay: delay.into(),
        }
    }
}

pub struct DelayDeviceControlInterface {
    config_tx: mpsc::UnboundedSender<DelayDeviceConfig>,
}

impl ControlInterface for DelayDeviceControlInterface {
    type Config = DelayDeviceConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Setting delay to {:?}", config.delay);
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct DelayDevice<P: Packet> {
    ingress: Arc<DelayDeviceIngress<P>>,
    egress: DelayDeviceEgress<P>,
    control_interface: Arc<DelayDeviceControlInterface>,
}

impl<P> Device<P> for DelayDevice<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = DelayDeviceIngress<P>;
    type EgressType = DelayDeviceEgress<P>;
    type ControlInterfaceType = DelayDeviceControlInterface;

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

impl<P> DelayDevice<P>
where
    P: Packet,
{
    pub fn new<D: Into<Option<Delay>>>(delay: D) -> Result<DelayDevice<P>, Error> {
        let delay = delay.into().unwrap_or_default();
        debug!(?delay, "New DelayDevice");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(DelayDevice {
            ingress: Arc::new(DelayDeviceIngress { ingress: rx }),
            egress: DelayDeviceEgress {
                egress: tx,
                delay,
                config_rx,
                timer: Timer::new()?,
            },
            control_interface: Arc::new(DelayDeviceControlInterface { config_tx }),
        })
    }
}

type DelayReplayDeviceIngress<P> = DelayDeviceIngress<P>;

pub struct DelayReplayDeviceEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<P>,
    trace: Box<dyn DelayTrace>,
    current_delay: Delay,
    next_available: Instant,
    next_change: Instant,
    config_rx: mpsc::UnboundedReceiver<DelayReplayDeviceConfig>,
    send_timer: Timer,
    change_timer: Timer,
    state: AtomicI32,
}

impl<P> DelayReplayDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    fn change_delay(&mut self, delay: Delay, change_time: Instant) {
        tracing::trace!(
            "Changing delay to {:?} (should at {:?} ago)",
            delay,
            change_time.elapsed()
        );
        tracing::trace!(
            "Previous next_available distance: {:?}",
            self.next_available - change_time
        );
        self.next_available += delay;
        self.next_available -= self.current_delay;
        tracing::trace!(
            before = ?self.current_delay,
            after = ?delay,
            "Set inner delay:"
        );
        tracing::trace!(
            "Now next_available distance: {:?}",
            self.next_available - change_time,
        );
        self.current_delay = delay;
    }

    fn set_config(&mut self, config: DelayReplayDeviceConfig) {
        tracing::debug!("Set inner trace config");
        self.trace = config.trace_config.into_model();
        let now = Instant::now();
        if self.next_change(now).is_none() {
            // handle null trace outside this function
            tracing::warn!("Setting null trace");
            self.next_change = now;
            // set state to 0 to indicate the trace goes to end and the device will drop all packets
            self.change_state(0);
        }
    }

    // Return the next change time or **None** if the trace goes to end
    fn next_change(&mut self, change_time: Instant) -> Option<()> {
        self.trace.next_delay().map(|(delay, duration)| {
            self.change_delay(delay, change_time);
            self.next_change = change_time + duration;
            tracing::trace!(
                "Delay changed to {:?}, next change after {:?}",
                delay,
                self.next_change - Instant::now()
            );
        })
    }
}

#[async_trait]
impl<P> Egress<P> for DelayReplayDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        let packet = loop {
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
                packet = self.egress.recv() => if let Some(packet) = packet { break packet }
            }
        };
        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            0 => {
                return None;
            }
            1 => {
                return Some(packet);
            }
            _ => {}
        }
        self.next_available = packet.get_timestamp() + self.current_delay;
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
                _ = self.send_timer.sleep(self.next_available - Instant::now()) => {
                    break;
                }
            }
        }
        Some(packet)
    }

    // This must be called before any dequeue
    fn reset(&mut self) {
        self.next_available = Instant::now();
        self.next_change = Instant::now();
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DelayReplayDeviceConfig {
    pub trace_config: Box<dyn DelayTraceConfig>,
}

impl Clone for DelayReplayDeviceConfig {
    fn clone(&self) -> Self {
        Self {
            trace_config: self.trace_config.clone(),
        }
    }
}

impl DelayReplayDeviceConfig {
    pub fn new<T: Into<Box<dyn DelayTraceConfig>>>(trace_config: T) -> Self {
        Self {
            trace_config: trace_config.into(),
        }
    }
}

pub struct DelayReplayDeviceControlInterface {
    config_tx: mpsc::UnboundedSender<DelayReplayDeviceConfig>,
}

impl ControlInterface for DelayReplayDeviceControlInterface {
    type Config = DelayReplayDeviceConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Setting delay replay config");
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct DelayReplayDevice<P: Packet> {
    ingress: Arc<DelayReplayDeviceIngress<P>>,
    egress: DelayReplayDeviceEgress<P>,
    control_interface: Arc<DelayReplayDeviceControlInterface>,
}

impl<P> Device<P> for DelayReplayDevice<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = DelayReplayDeviceIngress<P>;
    type EgressType = DelayReplayDeviceEgress<P>;
    type ControlInterfaceType = DelayReplayDeviceControlInterface;

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

impl<P> DelayReplayDevice<P>
where
    P: Packet,
{
    pub fn new(trace: Box<dyn DelayTrace>) -> Result<DelayReplayDevice<P>, Error> {
        tracing::debug!("New DelayReplayDevice");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(DelayReplayDevice {
            ingress: Arc::new(DelayReplayDeviceIngress { ingress: rx }),
            egress: DelayReplayDeviceEgress {
                egress: tx,
                trace,
                current_delay: Delay::ZERO,
                next_available: Instant::now(),
                next_change: Instant::now(),
                config_rx,
                send_timer: Timer::new()?,
                change_timer: Timer::new()?,
                state: AtomicI32::new(0),
            },
            control_interface: Arc::new(DelayReplayDeviceControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use netem_trace::model::{RepeatedDelayPatternConfig, StaticDelayConfig};
    use std::time::{Duration, Instant};
    use tracing::{span, Level};

    use crate::devices::StdPacket;

    use super::*;

    // The tolerance of the accuracy of the delays, in ms
    const DELAY_ACCURACY_TOLERANCE: f64 = 1.0;
    // List of delay times to be tested
    const DELAY_TEST_TIME: [u64; 8] = [0, 2, 5, 10, 20, 50, 100, 500];

    #[test_log::test]
    fn test_delay_device() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_delay_device").entered();
        for testing_delay in DELAY_TEST_TIME {
            let rt = tokio::runtime::Runtime::new()?;

            let _guard = rt.enter();

            info!("Creating device with {}ms delay", testing_delay);
            let device_config = DelayDeviceConfig::new(Duration::from_millis(testing_delay));
            let builder = device_config.into_factory::<StdPacket>();
            let device = builder(rt.handle())?;
            let ingress = device.sender();
            let mut egress = device.into_receiver();

            info!("Testing delay time for {}ms delay device", testing_delay);
            let mut delays: Vec<f64> = Vec::new();

            for _ in 0..10 {
                let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
                let start = Instant::now();
                ingress.enqueue(test_packet)?;
                let received = rt.block_on(async { egress.dequeue().await });

                // Use mircro second to get precision up to 0.001ms
                let duration = start.elapsed().as_micros() as f64 / 1000.0;

                delays.push(duration);

                // Should never loss packet
                assert!(received.is_some());

                let received = received.unwrap();

                // The length should be correct
                assert!(received.length() == 256);
            }

            info!(
                "Tested delays for {}ms delay device: {:?}",
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
    fn test_delay_device_config_update() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_delay_device_config_update").entered();
        let rt = tokio::runtime::Runtime::new()?;

        let _guard = rt.enter();

        info!("Creating device with 10ms delay");
        let device_config = DelayDeviceConfig::new(Duration::from_millis(10));
        let builder = device_config.into_factory::<StdPacket>();
        let device = builder(rt.handle())?;
        let config_changer = device.control_interface();
        let ingress = device.sender();
        let mut egress = device.into_receiver();

        //Test whether the packet will wait longer if the config is updated
        let test_packet = StdPacket::from_raw_buffer(&[0; 256]);

        let start = Instant::now();
        ingress.enqueue(test_packet)?;

        // Wait for 5ms, then change the config to let the delay be longer
        std::thread::sleep(Duration::from_millis(5));
        config_changer.set_config(DelayDeviceConfig::new(Duration::from_millis(20)))?;

        let received = rt.block_on(async { egress.dequeue().await });

        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay after update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);

        assert!((duration - 20.0).abs() <= DELAY_ACCURACY_TOLERANCE);

        // Test whether the packet will be returned immediately when the new delay is less than the already passed time
        let test_packet = StdPacket::from_raw_buffer(&[0; 256]);

        let start = Instant::now();
        ingress.enqueue(test_packet)?;

        // Wait for 15ms, then change the config back to 10ms
        std::thread::sleep(Duration::from_millis(15));
        config_changer.set_config(DelayDeviceConfig::new(Duration::from_millis(10)))?;

        let received = rt.block_on(async { egress.dequeue().await });

        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay after update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);

        assert!((duration - 15.0).abs() <= DELAY_ACCURACY_TOLERANCE);

        Ok(())
    }

    #[test_log::test]
    fn test_replay_delay_device() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_replay_delay_device").entered();
        let rt = tokio::runtime::Runtime::new()?;

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
        let device = DelayReplayDevice::new(delay_trace)?;
        let ingress = device.sender();
        let mut egress = device.into_receiver();
        egress.reset();
        egress.change_state(2);
        let start_time = tokio::time::Instant::now();
        let mut delays: Vec<f64> = Vec::new();
        for interval in [1100, 2100, 3100, 0] {
            for _ in 0..10 {
                let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
                let start = Instant::now();
                ingress.enqueue(test_packet)?;
                let received = rt.block_on(async { egress.dequeue().await });

                // Use mircro second to get precision up to 0.001ms
                let duration = start.elapsed().as_micros() as f64 / 1000.0;

                delays.push(duration);

                // Should never loss packet
                assert!(received.is_some());

                let received = received.unwrap();

                // The length should be correct
                assert!(received.length() == 256);
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
        }
        Ok(())
    }

    #[test_log::test]
    fn test_replay_delay_device_change_state() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_replay_delay_device").entered();
        let rt = tokio::runtime::Runtime::new()?;

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
        let device = DelayReplayDevice::new(delay_trace)?;
        let ingress = device.sender();
        let mut egress = device.into_receiver();
        egress.reset();
        let start_time = tokio::time::Instant::now();
        for _ in 0..10 {
            let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });
            // Should drop all packets
            assert!(received.is_none());
        }
        egress.change_state(1);
        let mut delays: Vec<f64> = Vec::new();
        rt.block_on(async {
            tokio::time::sleep_until(start_time + Duration::from_millis(1100)).await;
        });
        for _ in 0..10 {
            let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            let start = Instant::now();
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });

            // Use mircro second to get precision up to 0.001ms
            let duration = start.elapsed().as_micros() as f64 / 1000.0;

            delays.push(duration);

            // Should never loss packet
            assert!(received.is_some());

            let received = received.unwrap();

            // The length should be correct
            assert!(received.length() == 256);
        }
        for interval in [2100, 3100] {
            rt.block_on(async {
                tokio::time::sleep_until(start_time + Duration::from_millis(interval)).await;
            });
            for _ in 0..10 {
                let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
                let start = Instant::now();
                ingress.enqueue(test_packet)?;
                let received = rt.block_on(async { egress.dequeue().await });

                // Use mircro second to get precision up to 0.001ms
                let duration = start.elapsed().as_micros() as f64 / 1000.0;

                delays.push(duration);

                // Should never loss packet
                assert!(received.is_some());

                let received = received.unwrap();

                // The length should be correct
                assert!(received.length() == 256);
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
        }
        Ok(())
    }
}
