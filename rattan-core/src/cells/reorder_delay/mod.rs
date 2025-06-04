use std::sync::{atomic::AtomicI32, Arc};

use async_trait::async_trait;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::{sync::mpsc, time::Instant};
use tracing::debug;

use crate::{
    cells::{
        reorder_delay::{delay::DelayGenerator, queue::DelayQueue},
        Cell, ControlInterface, Egress, Ingress, Packet,
    },
    error::Error,
    metal::timer::Timer,
};

pub mod delay;
pub mod queue;

pub struct ReorderDelayCellIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Clone for ReorderDelayCellIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for ReorderDelayCellIngress<P>
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

pub struct ReorderDelayCellEgress<P, D>
where
    P: Packet,
    D: DelayGenerator,
{
    egress: mpsc::UnboundedReceiver<P>,
    delay: D,
    packet_queue: DelayQueue<P>,
    config_rx: mpsc::UnboundedReceiver<ReorderDelayCellConfig<D>>,
    timer: Timer,
    state: AtomicI32,
}

impl<P, D> ReorderDelayCellEgress<P, D>
where
    P: Packet + Send + Sync,
    D: DelayGenerator,
{
    fn set_config(&mut self, config: ReorderDelayCellConfig<D>) {
        self.delay = config.delay;
    }
}

#[async_trait]
impl<P, D> Egress<P> for ReorderDelayCellEgress<P, D>
where
    P: Packet + Send + Sync,
    D: DelayGenerator + Send,
{
    async fn dequeue(&mut self) -> Option<P> {
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
                                    self.packet_queue.enqueue(new_packet, self.delay.new_delay());
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
                                    self.packet_queue.enqueue(new_packet, self.delay.new_delay());
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
        let (instant, packet) = packet.expect("We cannot exit the previous loop if packet is None");
        if instant <= Instant::now() {
            Some(packet)
        } else {
            // We shouldn't have dequeue the packet, it is not ready to be sent yet.
            self.packet_queue.renqueue(packet, instant);
            None
        }
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }
}

#[cfg_attr(
    feature = "serde",
    serde_with::skip_serializing_none,
    derive(Deserialize, Serialize)
)]
#[derive(Debug)]
pub struct ReorderDelayCellConfig<D>
where
    D: DelayGenerator,
{
    pub delay: D,
}

impl<D> Clone for ReorderDelayCellConfig<D>
where
    D: DelayGenerator + Clone,
{
    fn clone(&self) -> Self {
        Self {
            delay: self.delay.clone(),
        }
    }
}

impl<D> ReorderDelayCellConfig<D>
where
    D: DelayGenerator,
{
    pub fn new(delay: impl Into<D>) -> Self {
        Self {
            delay: delay.into(),
        }
    }
}

pub struct ReorderDelayCellControlInterface<D>
where
    D: DelayGenerator,
{
    config_tx: mpsc::UnboundedSender<ReorderDelayCellConfig<D>>,
}

#[cfg(feature = "serde")]
impl<D> ControlInterface for ReorderDelayCellControlInterface<D>
where
    D: DelayGenerator + Send + Sync + for<'a> Deserialize<'a> + 'static,
{
    type Config = ReorderDelayCellConfig<D>;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

#[cfg(not(feature = "serde"))]
impl<D> ControlInterface for ReorderDelayCellControlInterface<D>
where
    D: DelayGenerator + Send + Sync + 'static,
{
    type Config = ReorderDelayCellConfig<D>;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct ReorderDelayCell<P: Packet, D: DelayGenerator> {
    ingress: Arc<ReorderDelayCellIngress<P>>,
    egress: ReorderDelayCellEgress<P, D>,
    control_interface: Arc<ReorderDelayCellControlInterface<D>>,
}

#[cfg(not(feature = "serde"))]
impl<P, D> Cell<P> for ReorderDelayCell<P, D>
where
    P: Packet + Send + Sync + 'static,
    D: DelayGenerator + Send + Sync + 'static,
{
    type IngressType = ReorderDelayCellIngress<P>;
    type EgressType = ReorderDelayCellEgress<P, D>;
    type ControlInterfaceType = ReorderDelayCellControlInterface<D>;

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

#[cfg(feature = "serde")]
impl<P, D> Cell<P> for ReorderDelayCell<P, D>
where
    P: Packet + Send + Sync + 'static,
    D: DelayGenerator + Send + Sync + 'static + for<'a> Deserialize<'a>,
{
    type IngressType = ReorderDelayCellIngress<P>;
    type EgressType = ReorderDelayCellEgress<P, D>;
    type ControlInterfaceType = ReorderDelayCellControlInterface<D>;

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

impl<P, D> ReorderDelayCell<P, D>
where
    P: Packet,
    D: DelayGenerator,
{
    pub fn new(delay: impl Into<D>) -> Result<ReorderDelayCell<P, D>, Error> {
        debug!("New ReorderDelayCell");
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(ReorderDelayCell {
            ingress: Arc::new(ReorderDelayCellIngress { ingress: rx }),
            egress: ReorderDelayCellEgress {
                egress: tx,
                delay: delay.into(),
                packet_queue: DelayQueue::new(),
                config_rx,
                timer: Timer::new()?,
                state: AtomicI32::new(0),
            },
            control_interface: Arc::new(ReorderDelayCellControlInterface { config_tx }),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};
    use tracing::{info, span, Level};

    use crate::cells::StdPacket;

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
            let cell = ReorderDelayCell::<_, Duration>::new(Duration::from_millis(testing_delay))?;
            let ingress = cell.sender();
            let mut egress = cell.into_receiver();
            egress.reset();
            egress.change_state(2);

            info!("Testing delay time for {}ms delay cell", testing_delay);
            let mut delays: Vec<f64> = Vec::new();

            for _ in 0..10 {
                let test_packet = StdPacket::from_raw_buffer(&[0; 256]);
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
    fn test_reorder_delay_cell_config_update() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_delay_cell_config_update").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let _guard = rt.enter();

        info!("Creating cell with 10ms delay");
        let cell = ReorderDelayCell::<_, Duration>::new(Duration::from_millis(10))?;
        let config_changer = cell.control_interface();
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        //Test whether the packet will wait longer if the config is updated
        let test_packet = StdPacket::from_raw_buffer(&[0; 256]);

        let start = Instant::now();
        ingress.enqueue(test_packet)?;

        // Wait for 5ms, then change the config to let the delay be longer
        std::thread::sleep(Duration::from_millis(5));
        config_changer.set_config(ReorderDelayCellConfig::new(Duration::from_millis(20)))?;

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
        config_changer.set_config(ReorderDelayCellConfig::new(Duration::from_millis(10)))?;

        let received = rt.block_on(async { egress.dequeue().await });

        let duration = start.elapsed().as_micros() as f64 / 1000.0;

        info!("Delay after update: {}ms", duration);

        assert!(received.is_some());
        let received = received.unwrap();
        assert!(received.length() == 256);

        assert!((duration - 15.0).abs() <= DELAY_ACCURACY_TOLERANCE);

        Ok(())
    }
}
