use std::sync::Arc;

use crate::{
    devices::Packet,
    metal::{error::MetalError, veth::VethDevice},
};

pub enum PacketType {
    PacketHost = 0,
    _PacketBroadcast = 1,
    _PacketMulticast = 2,
    PacketOtherhost = 3,
    _PacketOutgoing = 4,
}

pub trait InterfaceSender<P> {
    fn send(&self, packet: P) -> std::io::Result<()>;
    fn send_bulk(&self, packets: &[P]) -> std::io::Result<usize>;
}

pub trait InterfaceReceiver<P> {
    fn receive(&mut self) -> std::io::Result<Option<P>>;
    fn receive_bulk(&mut self, packets: &mut [P]) -> std::io::Result<usize>;
}

pub trait InterfaceDriver: Send + 'static {
    type Packet: Packet + Send;
    type Sender: InterfaceSender<Self::Packet>;
    type Receiver: InterfaceReceiver<Self::Packet>;

    fn bind_device(device: Arc<VethDevice>) -> Result<Self, MetalError>
    where
        Self: Sized;
    fn raw_fd(&self) -> i32;
    fn sender(&self) -> Arc<Self::Sender>;
    fn receiver(&mut self) -> &mut Self::Receiver;
}
