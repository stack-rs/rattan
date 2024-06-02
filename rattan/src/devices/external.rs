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
use async_trait::async_trait;
use nix::{
    sched::{sched_setaffinity, CpuSet},
    unistd::Pid,
};
#[cfg(feature = "serde")]
use serde::Deserialize;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};

use super::{ControlInterface, Egress, Ingress};

pub struct VirtualEthernetIngress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
{
    sender: tokio::sync::mpsc::Sender<D::Packet>,
}

impl<D> VirtualEthernetIngress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
{
    pub fn new(dev_sender: Vec<Arc<D::Sender>>) -> Self {
        let mut senders: Vec<tokio::sync::mpsc::Sender<D::Packet>> = vec![];
        let (dev_tx, dev_rx) = tokio::sync::mpsc::channel(1024);

        for s in dev_sender.iter() {
            let (tx, rx) = tokio::sync::mpsc::channel(1024);
            tokio::spawn(Self::send(rx, s.clone()));
            senders.push(tx);
        }

        tokio::spawn(Self::demux(dev_rx, senders.clone()));
        Self { sender: dev_tx }
    }

    async fn demux(
        mut receiver: tokio::sync::mpsc::Receiver<D::Packet>,
        senders: Vec<tokio::sync::mpsc::Sender<D::Packet>>,
    ) {
        loop {
            if let Some(packet) = receiver.recv().await {
                let _ = senders[(packet.flow_hash() as usize) % senders.len()]
                    .send(packet)
                    .await;
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
    token: CancellationToken,
}

impl<D> Drop for VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
{
    fn drop(&mut self) {
        self.token.cancel();
    }
}

impl<D> VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
{
    pub fn new(driver: Vec<D>, core: u32) -> Result<Self, Error> {
        let (tx, rx) = tokio::sync::mpsc::channel(1024);

        let receivers = driver
            .into_iter()
            .map(|d| (d.raw_fd(), d.into_receiver()))
            .collect();

        let token = CancellationToken::new();
        let cloned_token = token.clone();

        tokio::task::spawn_blocking(move || {
            Self::busy_polling_recv(receivers, tx, cloned_token, core);
        });

        Ok(Self {
            receiver: rx,
            token,
        })
    }

    fn busy_polling_recv(
        mut receivers: Vec<(i32, D::Receiver)>,
        sender: tokio::sync::mpsc::Sender<D::Packet>,
        token: CancellationToken,
        core: u32,
    ) {
        let mut cpuset = CpuSet::new();
        cpuset.set(core as usize).unwrap();
        sched_setaffinity(Pid::from_raw(0), &cpuset).unwrap();

        while !token.is_cancelled() {
            for (_, receiver) in receivers.iter_mut() {
                if let Ok(Some(packet)) = receiver.receive() {
                    let _ = sender.blocking_send(packet);
                }
            }
        }
    }
}

#[async_trait]
impl<D> Egress<D::Packet> for VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Receiver: Send,
    D::Packet: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<D::Packet> {
        self.receiver.recv().await
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
    pub fn new(device: Arc<VethDevice>, core: u32) -> Result<Self, Error> {
        debug!("New VirtualEthernet");
        let driver = D::bind_device(device.clone())?;
        let dev_senders = driver.iter().map(|d| d.sender()).collect();
        Ok(Self {
            _device: device,
            ingress: Arc::new(VirtualEthernetIngress::new(dev_senders)),
            egress: VirtualEthernetEgress::new(driver, core)?,
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
