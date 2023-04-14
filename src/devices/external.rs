use crate::{
    devices::{Device, Packet},
    error::Error,
    metal::{
        io::{InterfaceDriver, InterfaceReceiver, InterfaceSender},
        veth::VethDevice,
    },
};
/// External devices are all devices not created by Rattan. Network interfaces, physical or virtual, are
/// examples of external devices.
use std::{
    marker::PhantomData,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use tokio::io::unix::AsyncFd;

use super::{Egress, Ingress};

pub struct VirtualEthernetIngress<P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
{
    sender: Arc<D::Sender>,
}

impl<P, D> Ingress<P> for VirtualEthernetIngress<P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
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
                    Err(e) => eprintln!("recv error: {}", e),
                },
                Err(_would_block) => continue,
            }
        }
    }
}

pub struct VirtualEthernet<P: Packet, D: InterfaceDriver<P>>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
    D::Receiver: Send,
{
    _device: Arc<Mutex<VethDevice>>,
    ingress: Arc<VirtualEthernetIngress<P, D>>,
    egress: VirtualEthernetEgress<P, D>,
}

impl<P: Packet, D: InterfaceDriver<P>> VirtualEthernet<P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
    D::Receiver: Send,
{
    pub fn new(device: Arc<Mutex<VethDevice>>) -> Self {
        println!("create virtual ethernet");
        let driver = D::bind_device(device.clone()).unwrap();
        let notify = AsyncFd::new(driver.raw_fd()).unwrap();
        let sender_end = driver.sender();
        Self {
            _device: device,
            ingress: Arc::new(VirtualEthernetIngress { sender: sender_end }),
            egress: VirtualEthernetEgress {
                notify,
                driver,
                phantom: PhantomData,
            },
        }
    }
}

impl<P, D> Device<P> for VirtualEthernet<P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<P> + Send + Sync,
    D::Receiver: Send,
{
    type IngressType = VirtualEthernetIngress<P, D>;
    type EgressType = VirtualEthernetEgress<P, D>;

    fn sender(&self) -> Arc<Self::IngressType> {
        self.ingress.clone()
    }

    fn receiver(&mut self) -> &mut Self::EgressType {
        &mut self.egress
    }
}
