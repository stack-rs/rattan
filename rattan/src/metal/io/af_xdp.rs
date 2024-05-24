use camellia::{
    socket::af_xdp::{XskSocket, XskSocketBuilder},
    umem::{
        base::{UMem, UMemBuilder},
        frame::AppFrame,
        shared::SharedAccessorRef,
    },
};
use etherparse::{Ethernet2Header, Ipv4Header};
use once_cell::sync::Lazy;
use std::time::Duration;
use std::{
    io::ErrorKind,
    os::fd::{AsFd, AsRawFd},
    sync::{Arc, Mutex},
};
use tokio::{
    runtime::Handle,
    time::{sleep, Instant},
};
use tracing::debug;

static UMEM: Lazy<Arc<Mutex<UMem>>> = Lazy::new(|| {
    Arc::new(Mutex::new(
        UMemBuilder::new().num_chunks(32768).build().unwrap(),
    ))
});

use crate::{devices::Packet, metal::veth::VethDevice};

use super::common::{InterfaceDriver, InterfaceReceiver, InterfaceSender};

type XDPSocketRef = Arc<Mutex<XskSocket<SharedAccessorRef>>>;
pub struct XDPSender {
    xdp_socket: XDPSocketRef,
    device: Arc<VethDevice>,
    buffer: Vec<XDPPacket>,
}

type XDPSenderRef = Mutex<XDPSender>;

impl XDPSender {
    pub fn new(xdp_socket: XDPSocketRef, device: Arc<VethDevice>) -> XDPSender {
        XDPSender {
            xdp_socket,
            device,
            buffer: vec![],
        }
    }

    pub fn flush_buffer(&mut self) {
        let mut buffer = vec![];
        std::mem::swap(&mut buffer, &mut self.buffer);
        let _ = self.send_bulk(buffer.into_iter());
    }

    fn send(&mut self, mut packet: XDPPacket) -> std::io::Result<()> {
        let mut ether = packet.ether_hdr().unwrap();
        ether.source.copy_from_slice(&self.device.mac_addr.bytes());
        ether
            .destination
            .copy_from_slice(&self.device.peer().mac_addr.bytes());

        let buf = packet.as_raw_buffer();
        ether.write_to_slice(buf).unwrap();

        if self.buffer.len() > 32 {
            self.flush_buffer();
        } else {
            self.buffer.push(packet);
        }

        Ok(())
    }

    fn send_bulk<Iter, T>(&self, packets: Iter) -> std::io::Result<usize>
    where
        T: Into<XDPPacket>,
        Iter: IntoIterator<Item = T>,
        Iter::IntoIter: ExactSizeIterator,
    {
        let packets = packets.into_iter().map(|packet| {
            let mut packet: XDPPacket = packet.into();
            let mut ether = packet.ether_hdr().unwrap();
            ether.source.copy_from_slice(&self.device.mac_addr.bytes());
            ether
                .destination
                .copy_from_slice(&self.device.peer().mac_addr.bytes());

            let buf = packet.as_raw_buffer();
            ether.write_to_slice(buf).unwrap();
            packet.buf
        });

        let len = packets.len();

        let remaining = self
            .xdp_socket
            .lock()
            .unwrap()
            .send_bulk(packets)
            .map_err(|_| std::io::Error::new(ErrorKind::Other, "camellia error"))?;

        return Ok(len - remaining.len());
    }
}

impl InterfaceSender<XDPPacket> for XDPSenderRef {
    fn send(&self, packet: XDPPacket) -> std::io::Result<()> {
        self.lock().unwrap().send(packet)
    }

    fn send_bulk<Iter, T>(&self, packets: Iter) -> std::io::Result<usize>
    where
        T: Into<XDPPacket>,
        Iter: IntoIterator<Item = T>,
        Iter::IntoIter: ExactSizeIterator,
    {
        self.lock().unwrap().send_bulk(packets)
    }
}

pub struct XDPReceiver {
    xdp_socket: Arc<Mutex<XskSocket<SharedAccessorRef>>>,
    buffer: Vec<XDPPacket>,
}

impl InterfaceReceiver<XDPPacket> for XDPReceiver {
    fn receive(&mut self) -> std::io::Result<Option<XDPPacket>> {
        if let Some(p) = self.buffer.pop() {
            return Ok(Some(p));
        }
        let packet = self.receive_bulk()?;
        self.buffer.extend(packet);

        if self.buffer.len() == 0 {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "no packet",
            ))
        } else {
            Ok(self.buffer.pop())
        }
    }

    fn receive_bulk(&mut self) -> std::io::Result<Vec<XDPPacket>> {
        let packets: Vec<
            camellia::umem::frame::RxFrame<Arc<Mutex<camellia::umem::shared::SharedAccessor>>>,
        > = self.xdp_socket.lock().unwrap().recv_bulk(64).unwrap();

        Ok(packets
            .into_iter()
            .map(|x| AppFrame(x.0))
            .map(XDPPacket::from_frame)
            .collect())
    }
}

pub struct XDPDriver {
    sender: Arc<XDPSenderRef>,
    receiver: XDPReceiver,
    xdp_socket: Arc<Mutex<XskSocket<SharedAccessorRef>>>,
    _device: Arc<VethDevice>,
}

impl XDPDriver {
    async fn flush_buffer(sender: Arc<XDPSenderRef>) {
        loop {
            sleep(Duration::from_millis(10)).await;
            sender.lock().unwrap().flush_buffer();
        }
    }
}

impl InterfaceDriver for XDPDriver {
    type Packet = XDPPacket;
    type Sender = XDPSenderRef;
    type Receiver = XDPReceiver;

    fn bind_device(device: Arc<VethDevice>) -> Result<Self, crate::metal::error::MetalError>
    where
        Self: Sized,
    {
        debug!(?device, "bind device to AF_PACKET driver");

        let xdp_socket = Arc::new(Mutex::new(
            XskSocketBuilder::<SharedAccessorRef>::new()
                .ifname(&device.name)
                .queue_index(0)
                .with_umem(UMEM.clone())
                .enable_cooperate_schedule()
                .build_shared()?,
        ));

        let driver = XDPDriver {
            sender: Arc::new(Mutex::new(XDPSender {
                xdp_socket: xdp_socket.clone(),
                device: device.clone(),
                buffer: vec![],
            })),
            receiver: XDPReceiver {
                xdp_socket: xdp_socket.clone(),
                buffer: vec![],
            },
            xdp_socket,
            _device: device,
        };

        tokio::spawn(XDPDriver::flush_buffer(driver.sender.clone()));

        Ok(driver)
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

#[derive(Debug)]
pub struct XDPPacket {
    buf: AppFrame<SharedAccessorRef>,
    timestamp: Instant,
}

impl XDPPacket {
    fn from_frame(frame: AppFrame<SharedAccessorRef>) -> Self {
        XDPPacket {
            buf: frame,
            timestamp: Instant::now(),
        }
    }
}

impl Packet for XDPPacket {
    type PacketGenerator = XDPSocketRef;

    fn empty(_maximum: usize, _generator: &Self::PacketGenerator) -> Self {
        todo!()
    }

    fn from_raw_buffer(_buf: &[u8]) -> Self {
        todo!()
    }

    fn length(&self) -> usize {
        self.buf.len()
    }

    fn l2_length(&self) -> usize {
        self.buf.len()
    }

    fn l3_length(&self) -> usize {
        // 14 is the length of the Ethernet header
        self.buf.len() - 14
    }

    fn as_slice(&self) -> &[u8] {
        self.buf.raw_buffer()
    }

    fn as_raw_buffer(&mut self) -> &mut [u8] {
        self.buf.raw_buffer_mut()
    }

    fn ip_hdr(&self) -> Option<Ipv4Header> {
        if let Ok(result) = etherparse::Ethernet2Header::from_slice(self.as_slice()) {
            if let Ok(ip_hdr) = etherparse::Ipv4Header::from_slice(result.1) {
                return Some(ip_hdr.0);
            }
        }
        None
    }

    fn ether_hdr(&self) -> Option<Ethernet2Header> {
        etherparse::Ethernet2Header::from_slice(self.as_slice()).map_or(None, |x| Some(x.0))
    }

    fn get_timestamp(&self) -> Instant {
        self.timestamp
    }

    fn set_timestamp(&mut self, timestamp: Instant) {
        self.timestamp = timestamp;
    }
}
