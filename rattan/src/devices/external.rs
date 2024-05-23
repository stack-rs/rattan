use crate::{
    devices::{Device, Packet},
    error::Error,
    metal::{
        io::common::{InterfaceDriver, InterfaceReceiver, InterfaceSender},
        veth::VethDevice,
    },
};

/// External devices are all devices not created by Rattan.
/// Network interfaces, physical or virtual, are examples of external devices.
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
    sender: Arc<D::Sender>,
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
        self.sender.as_ref().send(packet).map_err(|e| e.into())
    }
}

pub struct VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
{
    notify: AsyncFd<i32>,
    driver: D,
}

#[async_trait]
impl<D> Egress<D::Packet> for VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Receiver: Send,
    D::Packet: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<D::Packet> {
        loop {
            let mut _guard = self.notify.readable().await.unwrap();
            match _guard.try_io(|_fd| self.driver.receiver().receive()) {
                Ok(packet) => match packet {
                    Ok(p) => return p,
                    Err(e) => error!("recv error: {}", e),
                },
                Err(_would_block) => continue,
            }
        }
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
    pub fn new(device: Arc<VethDevice>) -> Result<Self, Error> {
        debug!("New VirtualEthernet");
        let driver = D::bind_device(device.clone())?;
        let notify = AsyncFd::new(driver.raw_fd())?;
        let sender_end = driver.sender();
        Ok(Self {
            _device: device,
            ingress: Arc::new(VirtualEthernetIngress { sender: sender_end }),
            egress: VirtualEthernetEgress { notify, driver },
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
