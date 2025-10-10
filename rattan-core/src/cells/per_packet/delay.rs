use std::sync::{atomic::AtomicI32, Arc};

use async_trait::async_trait;
use netem_trace::{model::DelayPerPacketTraceConfig, DelayPerPacketTrace};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::{sync::mpsc, time::Instant};
use tracing::debug;

use crate::cells::per_packet::DelayedQueue;
use crate::{
    cells::{Cell, ControlInterface, Egress, Ingress, Packet},
    error::Error,
    metal::timer::Timer,
};

pub struct DelayPerPacketCellIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for DelayPerPacketCellIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for DelayPerPacketCellIngress<P>
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

pub struct DelayPerPacketCellEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<P>,
    delay: Box<dyn DelayPerPacketTrace>,
    packet_queue: DelayedQueue<P>,
    config_rx: mpsc::UnboundedReceiver<DelayPerPacketCellConfig>,
    timer: Timer,
    state: AtomicI32,
    notify_rx: Option<tokio::sync::broadcast::Receiver<crate::control::RattanNotify>>,
    started: bool,
}

impl<P> DelayPerPacketCellEgress<P>
where
    P: Packet + Send + Sync,
{
    fn set_config(&mut self, config: DelayPerPacketCellConfig) {
        self.delay = config.delay.into_model();
    }
}

#[async_trait]
impl<P> Egress<P> for DelayPerPacketCellEgress<P>
where
    P: Packet + Send + Sync,
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
                                    let Some(delay) = self.delay.next_delay() else { continue };
                                    self.packet_queue.enqueue(new_packet, delay);
                                }
                            }
                        }
                        None => {
                            // channel closed
                            return None;
                        }
                    }
                }
                _ = self.timer.sleep(self.packet_queue.next_instant() - Instant::now()) => {
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
                                    let Some(delay) = self.delay.next_delay() else { continue };
                                    self.packet_queue.enqueue(new_packet, delay);
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
        let (instant, mut packet) =
            packet.expect("We cannot exit the previous loop if packet is None");
        if instant <= Instant::now() {
            packet.delay_until(instant);
            Some(packet)
        } else {
            // We shouldn't have dequeue the packet, it is not ready to be sent yet.
            // self.packet_queue.renqueue(packet, instant);
            // None
            todo!()
        }
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
#[derive(Debug, Clone)]
pub struct DelayPerPacketCellConfig {
    pub delay: Box<dyn DelayPerPacketTraceConfig>,
}

impl DelayPerPacketCellConfig {
    pub fn new(delay: impl DelayPerPacketTraceConfig + 'static) -> Self {
        Self {
            delay: Box::new(delay),
        }
    }
}

pub struct DelayPerPacketCellControlInterface {
    config_tx: mpsc::UnboundedSender<DelayPerPacketCellConfig>,
}

impl ControlInterface for DelayPerPacketCellControlInterface {
    type Config = DelayPerPacketCellConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct DelayPerPacketCell<P: Packet> {
    ingress: Arc<DelayPerPacketCellIngress<P>>,
    egress: DelayPerPacketCellEgress<P>,
    control_interface: Arc<DelayPerPacketCellControlInterface>,
}

impl<P> Cell<P> for DelayPerPacketCell<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = DelayPerPacketCellIngress<P>;
    type EgressType = DelayPerPacketCellEgress<P>;
    type ControlInterfaceType = DelayPerPacketCellControlInterface;

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

impl<P> DelayPerPacketCell<P>
where
    P: Packet,
{
    pub fn new(
        config: impl Into<Box<dyn DelayPerPacketTrace>>,
    ) -> Result<DelayPerPacketCell<P>, Error> {
        debug!("New DelayPerPacketCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(DelayPerPacketCell {
            ingress: Arc::new(DelayPerPacketCellIngress { ingress: rx }),
            egress: DelayPerPacketCellEgress {
                egress: tx,
                delay: config.into(),
                packet_queue: DelayedQueue::new(),
                config_rx,
                timer: Timer::new()?,
                state: AtomicI32::new(0),
                notify_rx: None,
                started: false,
            },
            control_interface: Arc::new(DelayPerPacketCellControlInterface { config_tx }),
        })
    }
}

impl<Config: DelayPerPacketTraceConfig + 'static> From<Config> for DelayPerPacketCellConfig {
    fn from(value: Config) -> Self {
        Self {
            delay: Box::new(value),
        }
    }
}

// impl<Config: DelayPerPacketTraceConfig + 'static> From<Config> for DelayPerPacketCellConfig {
//     fn from(config: Config) -> Self {
//         Self {
//             delay: Box::new(config),
//         }
//     }
// }

#[cfg(test)]
mod tests {
    use netem_trace::{model::StaticDelayPerPacketConfig, Delay};
    use tokio::time::{Duration, Instant};
    use tracing::{info, span, Level};

    use crate::cells::{StdPacket, TestPacket};

    use super::*;

    // The tolerance of the accuracy of the delays, in ms
    const DELAY_ACCURACY_TOLERANCE: f64 = 1.0;
    // List of delay times to be tested
    const DELAY_TEST_TIME: [u64; 8] = [0, 2, 5, 10, 20, 50, 100, 500];

    #[test_log::test]
    fn test_reorder_delay_cell() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_delay_cell").entered();
        for testing_delay in DELAY_TEST_TIME {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            let _guard = rt.enter();

            info!("Creating cell with {}ms delay", testing_delay);
            let cell = DelayPerPacketCell::new(
                StaticDelayPerPacketConfig::new()
                    .delay(Delay::from_millis(testing_delay))
                    .build(),
            )?;
            let ingress = cell.sender();
            let mut egress = cell.into_receiver();
            egress.reset();
            egress.change_state(2);

            info!("Testing delay time for {}ms delay cell", testing_delay);
            let mut delays: Vec<f64> = Vec::new();
            let mut real_delays = Vec::new();

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

                real_delays.push(received.delay().as_micros() as f64 / 1000.0);
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

            let average_delay = delays.iter().sum::<f64>() / 10.0;
            debug!("Real Delays: {:?}", real_delays);
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
    fn test_reorder_delay_cell_config_update() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_delay_cell_config_update").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let _guard = rt.enter();

        info!("Creating cell with 10ms delay");
        let cell = DelayPerPacketCell::new(
            StaticDelayPerPacketConfig::new()
                .delay(Delay::from_millis(10))
                .build(),
        )?;
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
        config_changer.set_config(DelayPerPacketCellConfig::new(
            StaticDelayPerPacketConfig::new().delay(Delay::from_millis(20)),
        ))?;

        let received = rt.block_on(async { egress.dequeue().await });

        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay after update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);

        assert!((duration - 20.0).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert_eq!(received.delay(), Duration::from_millis(20));

        // Test whether the packet will be returned immediately when the new delay is less than the already passed time
        let test_packet = TestPacket::<StdPacket>::from_raw_buffer(&[0; 256], Instant::now());

        let start = Instant::now();
        ingress.enqueue(test_packet)?;

        // Wait for 15ms, then change the config back to 10ms
        std::thread::sleep(Duration::from_millis(15));
        config_changer.set_config(DelayPerPacketCellConfig::new(
            StaticDelayPerPacketConfig::new().delay(Delay::from_millis(10)),
        ))?;

        let received = rt.block_on(async { egress.dequeue().await });

        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay after update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);

        assert!((duration - 15.0).abs() <= DELAY_ACCURACY_TOLERANCE);
        assert_eq!(received.delay(), Duration::from_millis(15));

        Ok(())
    }
}
