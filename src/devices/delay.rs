use crate::devices::{Device, Packet};
use crate::error::Error;
use async_trait::async_trait;
use std::fmt::Debug;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration, Instant};

#[derive(Debug)]
struct DelayedPacket<P>
where
    P: Packet,
{
    ingress_time: Instant,
    packet: P,
}

pub struct DelayDevice<P: Packet> {
    ingress: mpsc::UnboundedSender<DelayedPacket<P>>,
    egress: mpsc::UnboundedReceiver<DelayedPacket<P>>,
}

#[async_trait]
impl<P> Device<P> for DelayDevice<P>
where
    P: Packet + Send + Sync,
{
    fn enqueue(&mut self, packet: P) -> Result<(), Error> {
        // XXX(minhuw): handle possible error here
        self.ingress
            .send(DelayedPacket {
                ingress_time: Instant::now(),
                packet,
            })
            .unwrap();
        Ok(())
    }

    async fn dequeue(&mut self) -> Option<P> {
        let packet = self.egress.recv().await.unwrap();
        let queuing_delay = Instant::now() - packet.ingress_time;
        if queuing_delay < Duration::from_micros(100) {
            sleep(Duration::from_micros(100) - queuing_delay).await;
        }
        Some(packet.packet)
    }
}

impl<P> DelayDevice<P>
where
    P: Packet,
{
    pub fn new() -> DelayDevice<P> {
        let (rx, tx) = mpsc::unbounded_channel();
        DelayDevice {
            ingress: rx,
            egress: tx,
        }
    }
}

impl<P> Default for DelayDevice<P>
where
    P: Packet,
{
    fn default() -> Self {
        Self::new()
    }
}
