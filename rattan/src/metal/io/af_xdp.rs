use std::{
    os::fd::{AsFd, AsRawFd},
    sync::{Arc, Mutex},
};

use camellia::{
    socket::af_xdp::{XskSocket, XskSocketBuilder},
    umem::{base::UMemBuilder, shared::SharedAccessor},
};
use tracing::debug;

use crate::{devices::Packet, metal::veth::VethDevice};

use super::common::{InterfaceDriver, InterfaceReceiver, InterfaceSender};

pub struct XDPSender {
    xdp_socket: Arc<Mutex<XskSocket<SharedAccessor>>>,
    device: Arc<VethDevice>,
}

impl<P> InterfaceSender<P> for XDPSender
where
    P: Packet,
{
    fn send(&self, mut packet: P) -> std::io::Result<()> {
        let mut ether = packet.ether_hdr().unwrap();
        ether.source.copy_from_slice(&self.device.mac_addr.bytes());
        ether
            .destination
            .copy_from_slice(&self.device.peer().mac_addr.bytes());

        let buf = packet.as_raw_buffer();
        ether.write_to_slice(buf).unwrap();

        let mut frame = self
            .xdp_socket
            .lock()
            .unwrap()
            .allocate(1)
            .unwrap()
            .pop()
            .unwrap();

        // TODO(minhuw): error handling here
        frame
            .raw_buffer_append(buf.len())
            .unwrap()
            .copy_from_slice(buf);

        self.xdp_socket.lock().unwrap().send(frame).unwrap();
        Ok(())
    }
}

pub struct XDPReceiver {
    xdp_socket: Arc<Mutex<XskSocket<SharedAccessor>>>,
}

impl<P> InterfaceReceiver<P> for XDPReceiver
where
    P: Packet,
{
    fn receive(&mut self) -> std::io::Result<Option<P>> {
        let packet = self.xdp_socket.lock().unwrap().recv().unwrap();
        if let Some(p) = packet {
            return Ok(Some(P::from_raw_buffer(p.raw_buffer())));
        }
        Ok(None)
    }
}

pub struct XDPDriver {
    sender: Arc<XDPSender>,
    receiver: XDPReceiver,
    xdp_socket: Arc<Mutex<XskSocket<SharedAccessor>>>,
    _device: Arc<VethDevice>,
}

impl<P> InterfaceDriver<P> for XDPDriver
where
    P: Packet,
{
    type Sender = XDPSender;
    type Receiver = XDPReceiver;

    fn bind_device(device: Arc<VethDevice>) -> Result<Self, crate::metal::error::MetalError>
    where
        Self: Sized,
    {
        debug!(?device, "bind device to AF_PACKET driver");
        let umem = Arc::new(Mutex::new(
            UMemBuilder::new().num_chunks(16384).build().unwrap(),
        ));

        let xdp_socket = Arc::new(Mutex::new(
            XskSocketBuilder::<SharedAccessor>::new()
                .ifname(&device.name)
                .queue_index(0)
                .with_umem(umem)
                .enable_cooperate_schedule()
                .build_shared()?,
        ));

        Ok(XDPDriver {
            sender: Arc::new(XDPSender {
                xdp_socket: xdp_socket.clone(),
                device: device.clone(),
            }),
            receiver: XDPReceiver {
                xdp_socket: xdp_socket.clone(),
            },
            xdp_socket,
            _device: device,
        })
    }

    fn raw_fd(&self) -> i32 {
        self.xdp_socket.lock().unwrap().as_fd().as_raw_fd()
    }

    fn sender(&self) -> Arc<Self::Sender> {
        self.sender.clone()
    }

    fn receiver(&mut self) -> &mut Self::Receiver {
        &mut self.receiver
    }
}
