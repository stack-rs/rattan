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
use std::{marker::PhantomData, sync::Arc};

use async_trait::async_trait;
#[cfg(feature = "serde")]
use serde::Deserialize;
use tokio::io::unix::AsyncFd;
use tracing::{debug, error, instrument};

use super::{ControlInterface, Egress, Ingress};

pub struct VirtualEthernetIngress<P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
    D::Sender: Send + Sync,
{
    sender: Arc<D::Sender>,
}

impl<P, D> Clone for VirtualEthernetIngress<P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
    D::Sender: Send + Sync,
{
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

impl<P, D> Ingress<P> for VirtualEthernetIngress<P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
    D::Sender: Send + Sync,
{
    fn enqueue(&self, packet: P) -> Result<(), Error> {
        self.sender.as_ref().send(packet).map_err(|e| e.into())
    }
}

pub struct VirtualEthernetEgress<P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
{
    notify: AsyncFd<i32>,
    driver: D,
    phantom: PhantomData<P>,
}

#[async_trait]
impl<P, D> Egress<P> for VirtualEthernetEgress<P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
    D::Receiver: Send,
{
    async fn dequeue(&mut self) -> Option<P> {
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

pub struct VirtualEthernet<P: Packet, D: InterfaceDriver<P>>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
{
    _device: Arc<VethDevice>,
    ingress: Arc<VirtualEthernetIngress<P, D>>,
    egress: VirtualEthernetEgress<P, D>,
    control_interface: Arc<VirtualEthernetControlInterface>,
}

impl<P: Packet, D: InterfaceDriver<P>> VirtualEthernet<P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
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
            egress: VirtualEthernetEgress {
                notify,
                driver,
                phantom: PhantomData,
            },
            control_interface: Arc::new(VirtualEthernetControlInterface {
                _config: VirtualEthernetConfig {},
            }),
        })
    }
}

impl<P, D> Device<P> for VirtualEthernet<P, D>
where
    P: Packet + Send + Sync + 'static,
    D: InterfaceDriver<P> + Send + Sync + 'static,
    D::Sender: Send + Sync,
    D::Receiver: Send,
{
    type IngressType = VirtualEthernetIngress<P, D>;
    type EgressType = VirtualEthernetEgress<P, D>;
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
