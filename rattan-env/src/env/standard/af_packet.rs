use std::{
    mem, ptr,
    sync::{Arc, Mutex},
};

use libc::{c_void, size_t, sockaddr, sockaddr_ll, socklen_t};
use nix::{
    errno::Errno,
    sys::socket::{AddressFamily, SockFlag, SockType},
};
use rattan_log::FlowDesc;
use tokio::{
    runtime::Handle,
    time::{Duration, Instant},
};
use tracing::{debug, error, trace, warn};

use crate::common::*;
use crate::env::standard::VethLikeDriver;

pub struct AfPacketSender {
    raw_fd: Mutex<i32>,
    cell: Arc<VethCell>,
}

impl InterfaceSender<StdPacket> for AfPacketSender {
    fn send(&self, mut packet: StdPacket) -> std::io::Result<()> {
        let peer_address = { self.cell.peer().mac_addr };

        let mut target_interface = libc::sockaddr_ll {
            sll_family: libc::AF_PACKET as u16,
            sll_protocol: packet.ether_hdr().unwrap().ether_type.0,
            sll_ifindex: self.cell.index as i32,
            sll_hatype: 0,
            sll_pkttype: 0,
            sll_halen: peer_address.bytes().len() as u8,
            sll_addr: [0; 8],
        };

        // TODO(minhuw): modify the raw buffer is not a good idea, especially when we need to
        // modify deeper headers, such as TCP header. Not sure whether `etherparse` can support
        // inplace update of packet. Maybe a home made packet parser and manipulation library is
        // necessary eventually.
        let mut ether = packet.ether_hdr().unwrap();
        ether.source.copy_from_slice(&self.cell.mac_addr.bytes());
        ether
            .destination
            .copy_from_slice(&self.cell.peer().mac_addr.bytes());

        let buf = packet.as_raw_buffer();
        ether.write_to_slice(buf).unwrap();

        unsafe {
            ptr::copy(
                peer_address.bytes().as_ptr(),
                target_interface.sll_addr.as_mut_ptr(),
                peer_address.bytes().len(),
            );

            let raw_fd = self.raw_fd.lock().unwrap();
            let _ret = Errno::result(libc::sendto(
                *raw_fd,
                buf.as_ptr() as *mut c_void,
                buf.len() as size_t,
                0,
                &target_interface as *const sockaddr_ll as *const sockaddr,
                std::mem::size_of_val(&target_interface) as u32,
            ))? as usize;
        }
        Ok(())
    }

    fn send_bulk<Iter, T>(&self, packets: Iter) -> std::io::Result<usize>
    where
        T: Into<StdPacket>,
        Iter: IntoIterator<Item = T>,
        Iter::IntoIter: ExactSizeIterator,
    {
        let mut count = 0;
        for packet in packets {
            self.send(packet.into())?;
            count += 1;
        }
        Ok(count)
    }
}

pub struct AfPacketReceiver {
    raw_fd: i32,
}

impl InterfaceReceiver<StdPacket> for AfPacketReceiver {
    fn receive(&mut self) -> std::io::Result<Option<StdPacket>> {
        let mut sockaddr = mem::MaybeUninit::<libc::sockaddr_ll>::uninit();
        let mut len = mem::size_of_val(&sockaddr) as socklen_t;

        let buf: mem::MaybeUninit<[u8; 65537]> = mem::MaybeUninit::uninit();

        // Safety: return value (`ret`) of libc::recvfrom is used to mark the initialized part of buffer, which is
        // exposed out of this function.
        let buf = unsafe { buf.assume_init() };

        let (ret, addr_ll) = unsafe {
            let ret = Errno::result(libc::recvfrom(
                self.raw_fd,
                buf.as_ptr() as *mut c_void,
                buf.len() as size_t,
                0,
                sockaddr.as_mut_ptr() as *mut libc::sockaddr,
                &mut len as *mut socklen_t,
            ))? as usize;

            let addr_ll = sockaddr_ll_from_raw(
                &sockaddr.assume_init() as *const libc::sockaddr_ll as *const libc::sockaddr,
                Some(len),
            )
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unable to get valid sockaddr_ll from AF_PACKET socket",
            ))?;

            (ret, addr_ll)
        };

        // accept input unicast packets only, ignore all outgoing and loopback packets
        if addr_ll.sll_pkttype != PacketType::PacketHost as u8
            && addr_ll.sll_pkttype != PacketType::PacketOtherhost as u8
        {
            trace!(
                header = ?format!("{:X?}", &buf[0..std::cmp::min(56, ret)]),
                "Ignore a packet from AF_PACKET {} (protocol: {:<04X}, pkttype: {}, source index: {}, hardware_addr: {:<02X}:{:<02X}:{:<02X}:{:<02X}:{:<02X}:{:<02X})",
                self.raw_fd,
                addr_ll.sll_protocol,
                addr_ll.sll_pkttype, addr_ll.sll_ifindex,
                addr_ll.sll_addr[0], addr_ll.sll_addr[1], addr_ll.sll_addr[2], addr_ll.sll_addr[3], addr_ll.sll_addr[4], addr_ll.sll_addr[5]
            );
            Ok(None)
        } else {
            trace!(
                header = ?format!("{:X?}", &buf[0..std::cmp::min(56, ret)]),
                "Receive a packet from AF_PACKET {} (protocol: {:<04X}, pkttype: {}, source index: {}, hardware_addr: {:<02X}:{:<02X}:{:<02X}:{:<02X}:{:<02X}:{:<02X})",
                self.raw_fd,
                addr_ll.sll_protocol,
                addr_ll.sll_pkttype, addr_ll.sll_ifindex,
                addr_ll.sll_addr[0], addr_ll.sll_addr[1], addr_ll.sll_addr[2], addr_ll.sll_addr[3], addr_ll.sll_addr[4], addr_ll.sll_addr[5]
            );
            Ok(Some(StdPacket::from_raw_buffer(&buf[0..ret])))
        }
    }

    fn receive_bulk(&mut self) -> std::io::Result<Vec<StdPacket>> {
        if let Some(x) = self.receive()? {
            Ok(vec![x])
        } else {
            Ok(vec![])
        }
    }
}

pub struct AfPacketDriver {
    raw_fd: i32,
    sender: Arc<AfPacketSender>,
    receiver: AfPacketReceiver,
    _cell: Arc<VethCell>,
}

impl VethLikeDriver for AfPacketDriver {
    fn bind_cell(cell: Arc<VethCell>, _runtime: &Handle) -> Result<Vec<Self>, MetalError> {
        debug!(?cell, "bind cell to AF_PACKET driver");
        let mut times = 3;
        let mut raw_fd;
        loop {
            let res = {
                raw_fd = unsafe {
                    Errno::result(libc::socket(
                        AddressFamily::Packet as libc::c_int,
                        SockType::Raw as libc::c_int
                            | SockFlag::SOCK_NONBLOCK.bits() as libc::c_int,
                        (libc::ETH_P_ALL as u16).to_be() as i32,
                    ))?
                };

                // It should work after this fix (https://github.com/nix-rust/nix/pull/1925) is available
                // let raw_socket = socket(
                //     AddressFamily::Packet,
                //     SockType::Raw,
                //     SockFlag::empty(),
                //     SockProtocol::EthAll
                // ).unwrap();

                debug!(
                    "create AF_PACKET socket {} on interface ({}:{})",
                    raw_fd, cell.name, cell.index
                );

                let bind_interface = libc::sockaddr_ll {
                    sll_family: libc::AF_PACKET as u16,
                    sll_protocol: (libc::ETH_P_ALL as u16).to_be(),
                    sll_ifindex: cell.index as i32,
                    sll_hatype: 0,
                    sll_pkttype: 0,
                    sll_halen: 0,
                    sll_addr: [0; 8],
                };

                unsafe {
                    Errno::result(libc::bind(
                        raw_fd,
                        &bind_interface as *const sockaddr_ll as *const sockaddr,
                        std::mem::size_of::<sockaddr_ll>() as u32,
                    ))
                }
            };
            match res {
                Ok(_) => break,
                Err(e) => {
                    times -= 1;
                    if times <= 0 {
                        error!("bind cell failed: {}", e);
                        return Err(MetalError::SystemError(e));
                    } else {
                        warn!("bind cell failed (Retrys Remain: {}): {}", times, e);
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                }
            }
        }
        if times < 3 {
            warn!("bind cell success after {} times", 3 - times);
        }

        Ok(vec![Self {
            sender: Arc::new(AfPacketSender {
                raw_fd: Mutex::new(raw_fd),
                cell: cell.clone(),
            }),
            receiver: AfPacketReceiver { raw_fd },
            raw_fd,
            _cell: cell,
        }])
    }
}

impl InterfaceDriver for AfPacketDriver {
    type Packet = StdPacket;
    type Sender = AfPacketSender;
    type Receiver = AfPacketReceiver;

    fn raw_fd(&self) -> i32 {
        self.raw_fd
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

unsafe fn sockaddr_ll_from_raw(
    addr: *const libc::sockaddr,
    len: Option<libc::socklen_t>,
) -> Option<libc::sockaddr_ll> {
    if let Some(l) = len {
        if l != mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t {
            return None;
        }
    }
    if (*addr).sa_family as i32 != libc::AF_PACKET {
        return None;
    }

    Some(ptr::read_unaligned(addr as *const _))
}

#[derive(Clone, Debug)]
pub struct StdPacket {
    pub buf: Vec<u8>,
    timestamp: Instant,
    flow_id: u32,
}

impl Packet for StdPacket {
    type PacketGenerator = ();

    fn empty(maximum: usize, _generator: &Self::PacketGenerator) -> Self {
        Self {
            buf: Vec::with_capacity(maximum),
            timestamp: Instant::now(),
            flow_id: 0,
        }
    }

    fn from_raw_buffer(buf: &[u8]) -> Self {
        Self {
            buf: buf.to_vec(),
            timestamp: Instant::now(),
            flow_id: 0,
        }
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
        self.buf.as_slice()
    }

    fn as_raw_buffer(&mut self) -> &mut [u8] {
        self.buf.as_mut_slice()
    }

    fn get_timestamp(&self) -> Instant {
        self.timestamp
    }

    fn set_timestamp(&mut self, timestamp: Instant) {
        self.timestamp = timestamp;
    }

    fn delay_by(&mut self, delay: Duration) {
        self.timestamp += delay;
    }

    fn delay_until(&mut self, timestamp: Instant) {
        self.timestamp = timestamp;
    }

    fn desc(&self) -> String {
        let mut desc = String::new();
        if let Ok(ether_hdr) = etherparse::Ethernet2HeaderSlice::from_slice(self.buf.as_slice()) {
            desc.push_str("[Ether] ");
            match ether_hdr.ether_type() {
                etherparse::EtherType::ARP => desc.push_str("[ARP]"),
                etherparse::EtherType::IPV4 => {
                    desc.push_str("[IPv4]");
                    match etherparse::Ipv4HeaderSlice::from_slice(
                        self.buf
                            .as_slice()
                            .get(ether_hdr.slice().len()..)
                            .unwrap_or(&[]),
                    ) {
                        Ok(ip_hdr) => {
                            desc.push_str(&format!(
                                " src: {} dst: {} id: {} offset: {} chksum: {} ",
                                ip_hdr.source_addr(),
                                ip_hdr.destination_addr(),
                                ip_hdr.identification(),
                                ip_hdr.fragments_offset(),
                                ip_hdr.header_checksum()
                            ));
                            match ip_hdr.protocol() {
                                etherparse::IpNumber::UDP => {
                                    desc.push_str("[UDP]");
                                    if let Ok(udp_hdr) = etherparse::UdpHeaderSlice::from_slice(
                                        self.buf
                                            .as_slice()
                                            .get(ether_hdr.slice().len() + ip_hdr.slice().len()..)
                                            .unwrap_or(&[]),
                                    ) {
                                        desc.push_str(&format!(
                                            " sport: {} dport: {} chksum: {}",
                                            udp_hdr.source_port(),
                                            udp_hdr.destination_port(),
                                            udp_hdr.checksum()
                                        ));
                                    }
                                }
                                etherparse::IpNumber::TCP => {
                                    desc.push_str("[TCP]");
                                    if let Ok(tcp_hdr) = etherparse::TcpHeaderSlice::from_slice(
                                        self.buf
                                            .as_slice()
                                            .get(ether_hdr.slice().len() + ip_hdr.slice().len()..)
                                            .unwrap_or(&[]),
                                    ) {
                                        desc.push_str(&format!(
                                            " sport: {} dport: {} chksum: {} seq: {} ack: {} flags: {}",
                                            tcp_hdr.source_port(),
                                            tcp_hdr.destination_port(),
                                            tcp_hdr.checksum(),
                                            tcp_hdr.sequence_number(),
                                            tcp_hdr.acknowledgment_number(),
                                            tcp_hdr.slice().get(13).unwrap()
                                        ));
                                    }
                                }
                                etherparse::IpNumber::ICMP => desc.push_str("[ICMP]"),
                                etherparse::IpNumber::IPV6_ICMP => desc.push_str("[IPV6_ICMP]"),
                                _ => desc.push_str("[Unknown]"),
                            }
                        }
                        Err(e) => {
                            desc.push_str(&format!("Error parsing: {e}"));
                        }
                    }
                }

                etherparse::EtherType::IPV6 => desc.push_str("[IPv6]"),
                _ => desc.push_str("[Unknown]"),
            }
        } else {
            desc.push_str("[Unknown]");
        }
        desc
    }

    fn flow_desc(&self) -> Option<FlowDesc> {
        if let Ok(ether_hdr) = etherparse::Ethernet2HeaderSlice::from_slice(self.buf.as_slice()) {
            match ether_hdr.ether_type() {
                etherparse::EtherType::IPV4 => {
                    match etherparse::Ipv4HeaderSlice::from_slice(
                        self.buf
                            .as_slice()
                            .get(ether_hdr.slice().len()..)
                            .unwrap_or(&[]),
                    ) {
                        Ok(ip_hdr) => match ip_hdr.protocol() {
                            etherparse::IpNumber::TCP => {
                                if let Ok(tcp_hdr) = etherparse::TcpHeaderSlice::from_slice(
                                    self.buf
                                        .as_slice()
                                        .get(ether_hdr.slice().len() + ip_hdr.slice().len()..)
                                        .unwrap_or(&[]),
                                ) {
                                    // Record all the options, only if SYN bit
                                    // is set (SYN/ SYN_ACK) packet.
                                    let options = tcp_hdr.syn().then(|| tcp_hdr.options().to_vec());
                                    Some(FlowDesc::TCP(
                                        ip_hdr.source_addr(),
                                        ip_hdr.destination_addr(),
                                        tcp_hdr.source_port(),
                                        tcp_hdr.destination_port(),
                                        options,
                                    ))
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        },
                        Err(_) => None,
                    }
                }
                _ => None,
            }
        } else {
            None
        }
    }

    fn set_flow_id(&mut self, flow_id: u32) {
        self.flow_id = flow_id;
    }

    fn get_flow_id(&self) -> u32 {
        self.flow_id
    }
}
