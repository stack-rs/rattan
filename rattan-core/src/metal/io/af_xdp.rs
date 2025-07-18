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
    collections::VecDeque,
    os::fd::{AsFd, AsRawFd},
    sync::Arc,
};
use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};
use tracing::debug;

static UMEM: Lazy<Arc<std::sync::Mutex<UMem>>> = Lazy::new(|| {
    Arc::new(std::sync::Mutex::new(
        UMemBuilder::new().num_chunks(32768).build().unwrap(),
    ))
});

use crate::{cells::Packet, metal::veth::VethCell};

use super::common::{InterfaceDriver, InterfaceReceiver, InterfaceSender};

type XDPSocketRef = Arc<Mutex<XskSocket<SharedAccessorRef>>>;

pub struct XDPSender {
    sender: tokio::sync::mpsc::Sender<XDPPacket>,
}

impl XDPSender {
    pub fn new(sender: tokio::sync::mpsc::Sender<XDPPacket>) -> XDPSender {
        XDPSender { sender }
    }
}

impl InterfaceSender<XDPPacket> for XDPSender {
    fn send(&self, packet: XDPPacket) -> std::io::Result<()> {
        // TODO(minhuw): handle errors more carefully here
        let _ = self.sender.try_send(packet);
        Ok(())
    }

    fn send_bulk<Iter, T>(&self, packets: Iter) -> std::io::Result<usize>
    where
        T: Into<XDPPacket>,
        Iter: IntoIterator<Item = T>,
        Iter::IntoIter: ExactSizeIterator,
    {
        let packets = packets.into_iter().map(|packet| packet.into());

        let len = packets.len();

        //TODO(minhuw): currently we return error even if part of packets are sent
        // maybe we should distinguish between partial success and total failure
        for packet in packets {
            self.sender
                .blocking_send(packet)
                .map_err(|_| std::io::Error::other("send error"))?;
        }

        Ok(len)
    }
}

pub struct XDPReceiver {
    xdp_socket: Arc<Mutex<XskSocket<SharedAccessorRef>>>,
    buffer: VecDeque<XDPPacket>,
}

impl InterfaceReceiver<XDPPacket> for XDPReceiver {
    fn receive(&mut self) -> std::io::Result<Option<XDPPacket>> {
        if let Some(p) = self.buffer.pop_front() {
            return Ok(Some(p));
        }
        let packet = self.receive_bulk()?;
        self.buffer.extend(packet);

        if self.buffer.is_empty() {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "no packet",
            ))
        } else {
            Ok(self.buffer.pop_front())
        }
    }

    fn receive_bulk(&mut self) -> std::io::Result<Vec<XDPPacket>> {
        match self.xdp_socket.try_lock() {
            Ok(mut xdp_socket) => {
                let packets = xdp_socket.recv_bulk(64).unwrap();

                if packets.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "no packet",
                    ));
                }
                Ok(packets
                    .into_iter()
                    .map(|x| AppFrame(x.0))
                    .map(XDPPacket::from_frame)
                    .collect())
            }
            Err(_) => Ok(vec![]),
        }
    }
}

pub struct XDPDriver {
    sender: Arc<XDPSender>,
    receiver: XDPReceiver,
    xdp_socket: XDPSocketRef,
}

impl XDPDriver {
    async fn buffered_send(
        mut receiver: tokio::sync::mpsc::Receiver<XDPPacket>,
        xdp_packet: XDPSocketRef,
        cell: Arc<VethCell>,
    ) {
        let mut packets = vec![];

        // TODO(minhuw): it should not leave forever. But let is be now.
        // we should stop the buffered send task when the cell exists.
        loop {
            tokio::select! {
                packet = receiver.recv() => {
                    if let Some(packet) = packet {
                        packets.push(packet);
                    }

                    if packets.len() >= 32 {
                        let mut send_packets = vec![];
                        std::mem::swap(&mut packets, &mut send_packets);
                        let _ = Self::send(&xdp_packet, &cell, send_packets).await;
                    }
                },
                _ = sleep(Duration::from_millis(10)) => {
                    let mut send_packets = vec![];
                    std::mem::swap(&mut packets, &mut send_packets);
                    let _ = Self::send(&xdp_packet, &cell, send_packets).await;
                }
            }
        }
    }

    async fn send<Iter, T>(
        xdp_socket: &XDPSocketRef,
        cell: &Arc<VethCell>,
        packets: Iter,
    ) -> std::io::Result<usize>
    where
        T: Into<XDPPacket>,
        Iter: IntoIterator<Item = T>,
        Iter::IntoIter: ExactSizeIterator,
    {
        let packets = packets.into_iter().map(|packet| {
            let mut packet: XDPPacket = packet.into();
            let mut ether = packet.ether_hdr().unwrap();
            ether.source.copy_from_slice(&cell.mac_addr.bytes());
            ether
                .destination
                .copy_from_slice(&cell.peer().mac_addr.bytes());

            let buf = packet.as_raw_buffer();
            ether.write_to_slice(buf).unwrap();
            packet.buf
        });

        let len = packets.len();
        let remaining = xdp_socket
            .lock()
            .await
            .send_bulk(packets)
            .map_err(|_| std::io::Error::other("camellia error"))?;

        Ok(len - remaining.len())
    }
}

impl InterfaceDriver for XDPDriver {
    type Packet = XDPPacket;
    type Sender = XDPSender;
    type Receiver = XDPReceiver;

    fn bind_cell(cell: Arc<VethCell>) -> Result<Vec<Self>, crate::metal::error::MetalError>
    where
        Self: Sized,
    {
        debug!(?cell, "bind cell to AF_PACKET driver");

        let xdp_socket = Arc::new(Mutex::new(
            XskSocketBuilder::<SharedAccessorRef>::new()
                .ifname(&cell.name)
                .queue_index(0)
                .with_umem(UMEM.clone())
                .enable_cooperate_schedule()
                .build_shared()?,
        ));

        let (sender, receiver) = tokio::sync::mpsc::channel(64);

        tokio::spawn(XDPDriver::buffered_send(
            receiver,
            xdp_socket.clone(),
            cell.clone(),
        ));

        Ok(vec![XDPDriver {
            sender: Arc::new(XDPSender { sender }),
            receiver: XDPReceiver {
                xdp_socket: xdp_socket.clone(),
                buffer: vec![].into(),
            },
            xdp_socket,
        }])
    }

    fn raw_fd(&self) -> i32 {
        self.xdp_socket.blocking_lock().as_fd().as_raw_fd()
    }

    fn sender(&self) -> Arc<Self::Sender> {
        self.sender.clone()
    }

    fn receiver(&mut self) -> &mut Self::Receiver {
        &mut self.receiver
    }

    fn into_receiver(self) -> Self::Receiver {
        self.receiver
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

    fn from_raw_buffer(_buf: &[u8], _timestamp: Instant) -> Self {
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

    fn delay_until(&mut self, timestamp: Instant) {
        self.timestamp = timestamp;
    }

    fn delay_by(&mut self, delay: Duration) {
        self.timestamp += delay;
    }
}
