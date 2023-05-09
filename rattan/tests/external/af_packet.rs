/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test af_packet -- --ignored --nocapture
use anyhow;
use libc::{c_void, size_t, sockaddr, sockaddr_ll, socklen_t};
use nix::errno::Errno;
use nix::sys::epoll::{epoll_create, epoll_ctl, epoll_wait, EpollEvent, EpollFlags};
use nix::sys::socket::{AddressFamily, SockType};
use rattan::env::{get_std_env, StdNetEnvConfig};
use rattan::metal::veth::{MacAddr, VethDevice};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::{mem, ptr};

fn create_dev_socket(veth: &VethDevice) -> anyhow::Result<i32> {
    let raw_socket = unsafe {
        Errno::result(libc::socket(
            AddressFamily::Packet as libc::c_int,
            SockType::Datagram as libc::c_int,
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
        raw_socket, veth.name, veth.index
    );

    let bind_interface = libc::sockaddr_ll {
        sll_family: libc::AF_PACKET as u16,
        sll_protocol: (libc::ETH_P_ALL as u16).to_be(),
        sll_ifindex: veth.index as i32,
        sll_hatype: 0,
        sll_pkttype: 0,
        sll_halen: 0,
        sll_addr: [0; 8],
    };

    unsafe {
        Errno::result(libc::bind(
            raw_socket,
            &bind_interface as *const libc::sockaddr_ll as *const libc::sockaddr,
            std::mem::size_of::<sockaddr_ll>() as u32,
        ))?;
    };

    Ok(raw_socket)
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

fn recv_from_dev(
    raw_fd: i32,
    buf: &mut [u8],
) -> anyhow::Result<(usize, Option<libc::sockaddr_ll>)> {
    let mut sockaddr = mem::MaybeUninit::<libc::sockaddr_ll>::uninit();
    let mut len = mem::size_of_val(&sockaddr) as socklen_t;

    unsafe {
        let ret = Errno::result(libc::recvfrom(
            raw_fd,
            buf.as_ptr() as *mut c_void,
            buf.len() as size_t,
            0,
            sockaddr.as_mut_ptr() as *mut libc::sockaddr,
            &mut len as *mut socklen_t,
        ))? as usize;

        Ok((
            ret,
            sockaddr_ll_from_raw(
                &sockaddr.assume_init() as *const libc::sockaddr_ll as *const libc::sockaddr,
                Some(len),
            ),
        ))
    }
}

fn send_to_dev(
    target_index: u32,
    target_addr: &MacAddr,
    source_hdr: &libc::sockaddr_ll,
    raw_fd: i32,
    buf: &mut [u8],
) -> anyhow::Result<usize> {
    unsafe {
        let mut target_interface = libc::sockaddr_ll {
            sll_family: libc::AF_PACKET as u16,
            sll_protocol: source_hdr.sll_protocol,
            sll_ifindex: target_index as i32,
            sll_hatype: 0,
            sll_pkttype: 0,
            sll_halen: target_addr.bytes().len() as u8,
            sll_addr: [0; 8],
        };

        ptr::copy(
            target_addr.bytes().as_ptr(),
            target_interface.sll_addr.as_mut_ptr(),
            target_addr.bytes().len(),
        );

        let ret = Errno::result(libc::sendto(
            raw_fd,
            buf.as_ptr() as *mut c_void,
            buf.len() as size_t,
            0,
            &target_interface as *const sockaddr_ll as *const sockaddr,
            std::mem::size_of_val(&target_interface) as u32,
        ))? as usize;

        Ok(ret)
    }
}

enum PacketType {
    PacketHost = 0,
    _PacketBroadcast = 1,
    _PacketMulticast = 2,
    _PacketOtherhost = 3,
    PacketOutgoing = 4,
}

#[test]
#[ignore]
fn af_packet_test() -> anyhow::Result<()> {
    let stdenv = get_std_env(StdNetEnvConfig::default()).unwrap();

    // step into rattan namespace
    {
        let netns = stdenv.rattan_ns;
        netns.enter().unwrap();

        let left_sniffer = create_dev_socket(&stdenv.left_pair.right).unwrap();
        let right_sniffer = create_dev_socket(&stdenv.right_pair.left).unwrap();
        println!(
            "left sniffer: {}, right sniffer: {}",
            left_sniffer, right_sniffer
        );
        let send_sock = unsafe {
            libc::socket(
                AddressFamily::Packet as libc::c_int,
                SockType::Datagram as libc::c_int,
                (libc::ETH_P_ALL as u16).to_be() as i32,
            )
        };

        let epoll_fd = epoll_create().unwrap();

        epoll_ctl(
            epoll_fd,
            nix::sys::epoll::EpollOp::EpollCtlAdd,
            left_sniffer,
            Some(&mut EpollEvent::new(
                EpollFlags::EPOLLIN,
                left_sniffer as u64,
            )),
        )
        .unwrap();

        epoll_ctl(
            epoll_fd,
            nix::sys::epoll::EpollOp::EpollCtlAdd,
            right_sniffer,
            Some(&mut EpollEvent::new(
                EpollFlags::EPOLLIN,
                right_sniffer as u64,
            )),
        )
        .unwrap();

        // Large enough to receive frame of localhost.
        let mut buf = [0u8; 65537];
        let mut events = [EpollEvent::empty(); 100];
        let timeout_ms = 1000;

        let running = Arc::new(AtomicBool::new(true));
        let rclone = running.clone();
        ctrlc::set_handler(move || {
            rclone.store(false, Ordering::Release);
        })
        .expect("unable to install ctrl+c handler");

        while running.load(Ordering::Acquire) {
            let num_events = epoll_wait(epoll_fd, &mut events, timeout_ms).unwrap();

            for i in 0..num_events {
                let fd = events[i].data() as i32;
                let (read, addr) = recv_from_dev(fd, &mut buf).expect("fail to recv");
                let addr = addr.unwrap();

                // ignore all outgoing packets
                if addr.sll_pkttype == PacketType::PacketOutgoing as u8
                    || addr.sll_pkttype == PacketType::PacketHost as u8
                {
                    continue;
                }

                // println!(
                //     "receive a packet (length: {}) from AF_PACKET {} (protocol: {:<04X}, pkttype: {}, source index: {}, hardware_addr: {:<02X}:{:<02X}:{:<02X}:{:<02X}:{:<02X}:{:<02X})",
                //     read, fd,
                //     addr.sll_protocol,
                //     addr.sll_pkttype, addr.sll_ifindex,
                //     addr.sll_addr[0], addr.sll_addr[1], addr.sll_addr[2], addr.sll_addr[3], addr.sll_addr[4], addr.sll_addr[5]
                // );

                match fd {
                    x if x == left_sniffer => {
                        assert_ne!(addr.sll_ifindex, stdenv.right_pair.left.index as i32);
                        send_to_dev(
                            stdenv.right_pair.left.index,
                            &stdenv.right_pair.right.mac_addr,
                            &addr,
                            send_sock,
                            &mut buf[0..read],
                        )?;
                        // println!("forward packet (length: {}) from left to right, from index {}, target index {}", size, addr.sll_ifindex, stdenv.right_pair.left.index)
                    }
                    x if x == right_sniffer => {
                        assert_ne!(addr.sll_ifindex, stdenv.left_pair.right.index as i32);
                        send_to_dev(
                            stdenv.left_pair.right.index,
                            &stdenv.left_pair.left.mac_addr,
                            &addr,
                            send_sock,
                            &mut buf[0..read],
                        )?;
                        // println!("forward packet (length: {}) from right to left, from index {}, target index {}", size, addr.sll_ifindex, stdenv.left_pair.right.index);
                    }
                    _ => panic!("unexpected fd: {}", fd),
                }
            }
        }
    }

    Ok(())
}
