use std::marker::PhantomData;

use crate::{
    devices::{Device, Packet},
    error::Error,
    metal::{io::InterfaceDriver, veth::VethDevice},
};
/// External devices are all devices not created by Rattan. Network interfaces, physical or virtual, are
/// examples of external devices.
use async_trait::async_trait;
use tokio::io::unix::AsyncFd;

pub struct VirtualEthernet<'a, P: Packet, D: InterfaceDriver<'a, P>> {
    _device: VethDevice,
    notify: AsyncFd<i32>,
    driver: D,
    _phantom: &'a PhantomData<P>,
}

#[async_trait]
impl<'a, P, D> Device<P> for VirtualEthernet<'a, P, D>
where
    P: Packet + Send + Sync,
    D: InterfaceDriver<'a, P> + Send + Sync,
{
    fn enqueue(&mut self, packet: P) -> Result<(), Error> {
        self.driver.send(packet).map_err(|e| e.into())
    }

    async fn dequeue(&mut self) -> Option<P> {
        let _guard = self.notify.readable().await.unwrap();
        Some(self.driver.receive().unwrap())
    }
}
