use std::{
    mem, ptr,
    sync::{Arc, Mutex},
};

use libc::{c_void, size_t, sockaddr, sockaddr_ll, socklen_t};
use nix::{
    errno::Errno,
    sys::socket::{AddressFamily, SockFlag, SockType},
};

use crate::devices::Packet;

use super::{error::MetalError, veth::VethDevice};

enum PacketType {
    PacketHost = 0,
    _PacketBroadcast = 1,
    _PacketMulticast = 2,
    _PacketOtherhost = 3,
    PacketOutgoing = 4,
}

pub trait InterfaceSender<P>
where
    P: Packet,
{
    fn send(&self, packet: P) -> std::io::Result<()>;
}

pub trait InterfaceReceiver<P>
where
    P: Packet,
{
    fn receive(&mut self) -> std::io::Result<Option<P>>;
}

pub trait InterfaceDriver<P>
where
    P: Packet,
{
    type Sender: InterfaceSender<P>;
    type Receiver: InterfaceReceiver<P>;

    fn bind_device(device: Arc<Mutex<VethDevice>>) -> Result<Self, MetalError>
    where
        Self: Sized;
    fn raw_fd(&self) -> i32;
    fn sender(&self) -> Arc<Self::Sender>;
    fn receiver(&mut self) -> &mut Self::Receiver;
}

pub struct AfPacketSender {
    raw_fd: Mutex<i32>,
    device: Arc<Mutex<VethDevice>>,
}

impl<P> InterfaceSender<P> for AfPacketSender
where
    P: Packet,
{
    fn send(&self, mut packet: P) -> std::io::Result<()> {
        let peer_address = {
            self.device
                .lock()
                .unwrap()
                .peer()
                .lock()
                .unwrap()
                .mac_addr
                .unwrap()
        };

        let mut target_interface = libc::sockaddr_ll {
            sll_family: libc::AF_PACKET as u16,
            sll_protocol: packet.ether_hdr().unwrap().ether_type,
            sll_ifindex: self.device.lock().unwrap().index as i32,
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
        ether
            .source
            .copy_from_slice(&self.device.lock().unwrap().mac_addr.unwrap().bytes());
        ether.destination.copy_from_slice(
            &self
                .device
                .lock()
                .unwrap()
                .peer()
                .lock()
                .unwrap()
                .mac_addr
                .unwrap()
                .bytes(),
        );

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
}

pub struct AfPacketReceiver {
    raw_fd: i32,
}

impl<P> InterfaceReceiver<P> for AfPacketReceiver
where
    P: Packet,
{
    fn receive(&mut self) -> std::io::Result<Option<P>> {
        let mut sockaddr = mem::MaybeUninit::<libc::sockaddr_ll>::uninit();
        let mut len = mem::size_of_val(&sockaddr) as socklen_t;

        let buf = [0u8; 65537];

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

        // ignore all outgoing and loopback packets
        if addr_ll.sll_pkttype == PacketType::PacketOutgoing as u8
            || addr_ll.sll_pkttype == PacketType::PacketHost as u8
        {
            Ok(None)
        } else {
            println!(
                "receive a packet from AF_PACKET {} (protocol: {:<04X}, pkttype: {}, source index: {}, hardware_addr: {:<02X}:{:<02X}:{:<02X}:{:<02X}:{:<02X}:{:<02X})",
                self.raw_fd,
                addr_ll.sll_protocol,
                addr_ll.sll_pkttype, addr_ll.sll_ifindex,
                addr_ll.sll_addr[0], addr_ll.sll_addr[1], addr_ll.sll_addr[2], addr_ll.sll_addr[3], addr_ll.sll_addr[4], addr_ll.sll_addr[5]
            );
            Ok(Some(P::from_raw_buffer(&buf[0..ret])))
        }
    }
}

pub struct AfPacketDriver {
    raw_fd: i32,
    sender: Arc<AfPacketSender>,
    receiver: AfPacketReceiver,
    _device: Arc<Mutex<VethDevice>>,
}

impl<P> InterfaceDriver<P> for AfPacketDriver
where
    P: Packet,
{
    type Sender = AfPacketSender;
    type Receiver = AfPacketReceiver;
    fn bind_device(device: Arc<Mutex<VethDevice>>) -> Result<Self, MetalError> {
        println!("bind device to AF_PACKET driver");
        let raw_fd = unsafe {
            Errno::result(libc::socket(
                AddressFamily::Packet as libc::c_int,
                SockType::Raw as libc::c_int | SockFlag::SOCK_NONBLOCK.bits() as libc::c_int,
                (libc::ETH_P_ALL as u16).to_be() as i32,
            ))?
        };
        {
            let device = device.lock().unwrap();
            // It should work after this fix (https://github.com/nix-rust/nix/pull/1925) is available
            // let raw_socket = socket(
            //     AddressFamily::Packet,
            //     SockType::Raw,
            //     SockFlag::empty(),
            //     SockProtocol::EthAll
            // ).unwrap();

            println!(
                "create AF_PACKET socket {} on interface ({}:{})",
                raw_fd, device.name, device.index
            );

            let bind_interface = libc::sockaddr_ll {
                sll_family: libc::AF_PACKET as u16,
                sll_protocol: (libc::ETH_P_ALL as u16).to_be(),
                sll_ifindex: device.index as i32,
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
                ))?;
            };
        }

        Ok(Self {
            sender: Arc::new(AfPacketSender {
                raw_fd: Mutex::new(raw_fd),
                device: device.clone(),
            }),
            receiver: AfPacketReceiver { raw_fd },
            raw_fd,
            _device: device,
        })
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
