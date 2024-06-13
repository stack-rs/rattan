/// External devices are all devices not created by Rattan.
/// Network interfaces, physical or virtual, are examples of external devices.
use crate::{
    devices::{Device, Packet},
    error::{Error, TokioRuntimeError},
    metal::{
        io::common::{InterfaceDriver, InterfaceReceiver, InterfaceSender},
        veth::VethDevice,
    },
};
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(feature = "serde")]
use serde::Deserialize;
use tokio::io::unix::AsyncFd;
use tracing::{debug, error, instrument};

use super::{ControlInterface, Egress, Ingress};

pub struct VirtualEthernetIngress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
{
    sender: tokio::sync::mpsc::Sender<D::Packet>,
    id: Arc<String>,
}

impl<D> VirtualEthernetIngress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
{
    pub fn new(dev_sender: Vec<Arc<D::Sender>>, id: Arc<String>) -> Self {
        let mut senders: Vec<tokio::sync::mpsc::Sender<D::Packet>> = vec![];
        let (dev_tx, dev_rx) = tokio::sync::mpsc::channel(1024);

        for s in dev_sender.iter() {
            let (tx, rx) = tokio::sync::mpsc::channel(1024);
            tokio::spawn(Self::send(rx, s.clone()));
            senders.push(tx);
        }

        tokio::spawn(Self::demux(dev_rx, senders.clone()));
        Self { sender: dev_tx, id }
    }

    async fn demux(
        mut receiver: tokio::sync::mpsc::Receiver<D::Packet>,
        senders: Vec<tokio::sync::mpsc::Sender<D::Packet>>,
    ) {
        let mut index: usize = 0;
        loop {
            if let Some(packet) = receiver.recv().await {
                let _ = senders[index % senders.len()].send(packet).await;
                index += 1;
            }
        }
    }

    async fn send(mut receiver: tokio::sync::mpsc::Receiver<D::Packet>, sender: Arc<D::Sender>) {
        loop {
            if let Some(packet) = receiver.recv().await {
                let _ = sender.send(packet);
            }
        }
    }
}

impl<D> Clone for VirtualEthernetIngress<D>
where
    D: InterfaceDriver + Send + Sync,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
{
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            id: self.id.clone(),
        }
    }
}

impl<D> Ingress<D::Packet> for VirtualEthernetIngress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
{
    fn enqueue(&self, packet: D::Packet) -> Result<(), Error> {
        let ts = get_clock_ns();
        tracing::trace!(target: "veth::ingress::packet", "At {} veth {} ingress enqueue pkt length {}", ts, self.id, packet.length());
        Ok(self
            .sender
            .try_send(packet)
            .map_err(|e| TokioRuntimeError::MpscError(e.to_string()))?)
    }
}

pub struct VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
{
    receiver: tokio::sync::mpsc::Receiver<D::Packet>,
    _id: String,
}

impl<D> VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
{
    pub fn new(driver: Vec<D>, id: String) -> Result<Self, Error> {
        let (tx, rx) = tokio::sync::mpsc::channel(1024);

        for d in driver.into_iter() {
            let notify = AsyncFd::new(d.raw_fd())?;
            tokio::spawn(Self::recv(
                id.clone(),
                notify,
                d.into_receiver(),
                tx.clone(),
            ));
        }

        Ok(Self {
            receiver: rx,
            _id: id,
        })
    }

    async fn recv(
        id: String,
        notify: AsyncFd<i32>,
        mut receiver: D::Receiver,
        sender: tokio::sync::mpsc::Sender<D::Packet>,
    ) {
        loop {
            let mut _guard = notify.readable().await.unwrap();
            match _guard.try_io(|_fd| receiver.receive()) {
                Ok(packet) => match packet {
                    Ok(Some(p)) => {
                        let ts = get_clock_ns();
                        tracing::trace!(target: "veth::egress::packet", "At {} veth {} egress dequeue pkt length {}", ts, id, p.length());
                        let _ = sender.send(p).await;
                    }
                    Err(e) => error!("recv error: {}", e),
                    _ => {}
                },
                Err(_would_block) => continue,
            }
        }
    }
}

fn get_clock_ns() -> i64 {
    nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)
        .map(|ts| ts.tv_sec() * 1_000_000_000 + ts.tv_nsec())
        .unwrap_or(0)
}

#[async_trait]
impl<D> Egress<D::Packet> for VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Receiver: Send,
    D::Packet: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<D::Packet> {
        return self.receiver.recv().await;
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize))]
#[derive(Debug, Clone)]
pub struct VirtualEthernetConfig {}

pub struct VirtualEthernetControlInterface {
    _config: VirtualEthernetConfig,
}

impl ControlInterface for VirtualEthernetControlInterface {
    type Config = VirtualEthernetConfig;
    fn set_config(&self, _config: VirtualEthernetConfig) -> Result<(), Error> {
        Ok(())
    }
}

pub struct VirtualEthernet<D: InterfaceDriver>
where
    D: InterfaceDriver + Send,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
{
    _device: Arc<VethDevice>,
    ingress: Arc<VirtualEthernetIngress<D>>,
    egress: VirtualEthernetEgress<D>,
    control_interface: Arc<VirtualEthernetControlInterface>,
}

impl<D: InterfaceDriver> VirtualEthernet<D>
where
    D: InterfaceDriver + Send,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
{
    #[instrument(skip_all, name="VirtualEthernet", fields(name = device.name))]
    pub fn new(device: Arc<VethDevice>, id: String) -> Result<Self, Error> {
        debug!("New VirtualEthernet");
        let driver = D::bind_device(device.clone())?;
        let dev_senders = driver.iter().map(|d| d.sender()).collect();
        Ok(Self {
            _device: device,
            ingress: Arc::new(VirtualEthernetIngress::new(
                dev_senders,
                Arc::new(id.clone()),
            )),
            egress: VirtualEthernetEgress::new(driver, id)?,
            control_interface: Arc::new(VirtualEthernetControlInterface {
                _config: VirtualEthernetConfig {},
            }),
        })
    }
}

impl<D> Device<D::Packet> for VirtualEthernet<D>
where
    D: InterfaceDriver + Send + 'static,
    D::Packet: Packet + Send + Sync + 'static,
    D::Sender: Send + Sync,
    D::Receiver: Send,
{
    type IngressType = VirtualEthernetIngress<D>;
    type EgressType = VirtualEthernetEgress<D>;
    type ControlInterfaceType = VirtualEthernetControlInterface;

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
