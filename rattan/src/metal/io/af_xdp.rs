use std::sync::{Arc, Mutex};

use crate::{devices::Packet, metal::veth::VethDevice};

use super::common::{InterfaceDriver, InterfaceReceiver, InterfaceSender};

pub struct XDPSender {
    _raw_fd: Mutex<i32>,
    _device: Arc<VethDevice>,
}

impl<P> InterfaceSender<P> for XDPSender
where
    P: Packet,
{
    fn send(&self, mut _packet: P) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct XDPReceiver {
    _raw_fd: i32,
}

impl<P> InterfaceReceiver<P> for XDPReceiver
where
    P: Packet,
{
    fn receive(&mut self) -> std::io::Result<Option<P>> {
        Ok(None)
    }
}

pub struct XDPDriver {
    raw_fd: i32,
    sender: Arc<XDPSender>,
    receiver: XDPReceiver,
    _device: Arc<VethDevice>,
}

impl<P> InterfaceDriver<P> for XDPDriver
where
    P: Packet,
{
    type Sender = XDPSender;
    type Receiver = XDPReceiver;

    fn bind_device(_device: Arc<VethDevice>) -> Result<Self, crate::metal::error::MetalError>
    where
        Self: Sized,
    {
        unimplemented!()
    }

    fn raw_fd(&self) -> i32 {
        self.raw_fd
    }

    fn sender(&self) -> Arc<Self::Sender> {
        self.sender.clone()
    }

    fn receiver(&mut self) -> &mut Self::Receiver {
        &mut self.receiver
    }
}
