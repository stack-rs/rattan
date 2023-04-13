use std::{mem, ptr};

use libc::{c_void, size_t, sockaddr, sockaddr_ll, socklen_t};
use nix::{
    errno::Errno,
    sys::socket::{AddressFamily, SockType},
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

pub trait InterfaceDriver<'b, P>
where
    P: Packet,
{
    fn bind_device<'a>(device: &'a mut VethDevice) -> Result<Self, MetalError>
    where
        'a: 'b,
        Self: Sized;
    fn send(&mut self, packet: P) -> Result<(), MetalError>;
    fn receive(&mut self) -> Result<P, MetalError>;
}

pub struct AfPacketDriver<'a> {
    raw_fd: i32,
    device: &'a mut VethDevice,
}

impl<'b, P> InterfaceDriver<'b, P> for AfPacketDriver<'b>
where
    P: Packet,
{
    fn bind_device<'a>(device: &'a mut VethDevice) -> Result<Self, MetalError>
    where
        'a: 'b,
    {
        let raw_fd = unsafe {
            Errno::result(libc::socket(
                AddressFamily::Packet as libc::c_int,
                SockType::Raw as libc::c_int,
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

        Ok(Self { raw_fd, device })
    }

    fn send(&mut self, packet: P) -> Result<(), MetalError> {
        let peer_address = &self.device.peer().lock().unwrap().mac_addr.unwrap();

        let mut target_interface = libc::sockaddr_ll {
            sll_family: libc::AF_PACKET as u16,
            sll_protocol: packet.ether_hdr().ether_type,
            sll_ifindex: self.device.index as i32,
            sll_hatype: 0,
            sll_pkttype: 0,
            sll_halen: peer_address.bytes().len() as u8,
            sll_addr: [0; 8],
        };

        // TODO(minhuw): modify the raw buffer is not a good idea, especially when we need to
        // modify deeper headers, such as TCP header. Not sure whether `etherparse` can support
        // inplace update of packet. Maybe a packet parser and manipulation library is necessary
        // eventually.
        let mut ether = packet.ether_hdr().clone();
        ether
            .source
            .copy_from_slice(&self.device.mac_addr.unwrap().bytes());
        ether
            .destination
            .copy_from_slice(&self.device.peer().lock().unwrap().mac_addr.unwrap().bytes());

        let buf = packet.as_raw_buffer();
        ether.write_to_slice(buf).unwrap();

        unsafe {
            ptr::copy(
                peer_address.bytes().as_ptr(),
                target_interface.sll_addr.as_mut_ptr(),
                peer_address.bytes().len(),
            );

            let _ret = Errno::result(libc::sendto(
                self.raw_fd,
                buf.as_ptr() as *mut c_void,
                buf.len() as size_t,
                0,
                &target_interface as *const sockaddr_ll as *const sockaddr,
                std::mem::size_of_val(&target_interface) as u32,
            ))? as usize;
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<P, MetalError> {
        let mut sockaddr = mem::MaybeUninit::<libc::sockaddr_ll>::uninit();
        let mut len = mem::size_of_val(&sockaddr) as socklen_t;

        let buf = [0u8; 65537];

        let (_ret, addr_ll) = unsafe {
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
            Err(MetalError::NotInterestedPacket)
        } else {
            Ok(P::from_raw_buffer(&buf))
        }
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
