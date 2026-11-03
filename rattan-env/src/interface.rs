use std::sync::Arc;

use crate::common::Packet;

pub trait InterfaceSender<P> {
    fn send(&self, packet: P) -> std::io::Result<()>;
    fn send_bulk<Iter, T>(&self, packets: Iter) -> std::io::Result<usize>
    where
        T: Into<P>,
        Iter: IntoIterator<Item = T>,
        Iter::IntoIter: ExactSizeIterator;
}

pub trait InterfaceReceiver<P> {
    fn receive(&mut self) -> std::io::Result<Option<P>>;
    fn receive_bulk(&mut self) -> std::io::Result<Vec<P>>;
}

pub trait InterfaceDriver: Send + 'static {
    type Packet: Packet + Send;
    type Sender: InterfaceSender<Self::Packet>;
    type Receiver: InterfaceReceiver<Self::Packet> + Send;

    fn raw_fd(&self) -> i32;
    fn sender(&self) -> Arc<Self::Sender>;
    fn receiver(&mut self) -> &mut Self::Receiver;
    fn into_receiver(self) -> Self::Receiver;
}

pub struct InterfaceBuildArtifact<D: InterfaceDriver> {
    pub ns_id: u8,
    pub veth_id: u8,
    pub name: String,
    pub drivers: Vec<D>,
}
