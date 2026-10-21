//! Test token semantics: chunk-bound tokens, CLONE, and misbehaving userspace
//!
//! Tokens are `gen << 32 | chunk_index`, one per UMEM chunk. This test checks
//! that encoding, and that every way userspace can misuse a token or a chunk
//! fails cleanly instead of corrupting kernel state:
//!
//! - READ_PAYLOAD / RELEASE / CLONE on live and on stale tokens
//! - CLONE onto a chunk that is still live (EBUSY) or misaligned (EINVAL)
//! - replaying a token, and putting one token on the TX ring twice
//! - releasing a token via DROP and transmitting it at the same time
//! - putting a live chunk back on FILL
//! - closing a device while its tokens are pending, then releasing them
//!   through the device that shares the UMEM
//!
//! Both devices stay in the root netns. Packets are UDP datagrams sent to a
//! permanent neighbour, so nothing but our own traffic reaches the rings.
//!
//! Run with: sudo ./target/release/examples/test_token
//! Assumes the kernel module is loaded with `num_queues=1`.

use rvnic::sys::RattanDesc;
use rvnic::{Rings, RvnicDevice, RxRing, UmemBuilder};
use std::net::UdpSocket;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

const CHUNK_SIZE: u64 = 2048;
const NUM_CHUNKS: u32 = 64;
const MARKER: &[u8] = b"rvnic-token-test";

// Chunks 0..28 go to rattan0's FILL. 30 and 31 are kept back for CLONE.
// rattan1's FILL is left empty until the live-chunk test.
const DEV0_FILL: u32 = 28;
const CLONE_CHUNK: u64 = 30 * CHUNK_SIZE;

fn sh(args: &[&str]) {
    let status = Command::new(args[0]).args(&args[1..]).status().unwrap();
    assert!(status.success(), "command failed: {:?}", args);
}

fn stat(iface: &str, name: &str) -> u64 {
    std::fs::read_to_string(format!("/sys/class/net/{}/statistics/{}", iface, name))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Packets NAPI handed to the stack on `iface`, accepted or not
fn delivered(iface: &str) -> u64 {
    stat(iface, "rx_packets") + stat(iface, "rx_dropped")
}

fn setup_iface(iface: &str, addr: &str, peer: &str) {
    sh(&[
        "sysctl",
        "-q",
        &format!("net.ipv6.conf.{}.disable_ipv6=1", iface),
    ]);
    sh(&["ip", "addr", "add", addr, "dev", iface]);
    sh(&["ip", "link", "set", iface, "up"]);
    sh(&[
        "ip",
        "neigh",
        "replace",
        peer,
        "lladdr",
        "02:00:00:00:00:02",
        "dev",
        iface,
        "nud",
        "permanent",
    ]);
}

fn send(src: &str, dst: &str, seq: u32) {
    let sock = UdpSocket::bind(format!("{}:0", src)).unwrap();
    let mut payload = MARKER.to_vec();
    payload.extend_from_slice(&seq.to_le_bytes());
    sock.send_to(&payload, format!("{}:9999", dst)).unwrap();
}

fn payload(dev: &RvnicDevice, token: u64) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; 2048];
    let (len, _total) = dev
        .read_payload(token, &mut buf, 0)
        .map_err(|e| std::io::Error::other(format!("{:?}", e)))?;
    buf.truncate(len as usize);
    Ok(buf)
}

fn is_ours(pkt: &[u8]) -> bool {
    pkt.windows(MARKER.len()).any(|w| w == MARKER)
}

/// Wait for `n` of our packets on `rx`, releasing anything else that shows up
fn recv(dev: &RvnicDevice, rx: &mut RxRing, n: usize) -> Vec<RattanDesc> {
    let mut out = Vec::new();
    let mut descs = [RattanDesc::default(); 16];
    let deadline = Instant::now() + Duration::from_secs(2);
    while out.len() < n {
        assert!(
            Instant::now() < deadline,
            "timed out, got {}/{} packets",
            out.len(),
            n
        );
        let got = rx.consume(&mut descs);
        for d in &descs[..got] {
            if is_ours(&payload(dev, d.token).unwrap()) {
                out.push(*d);
            } else {
                dev.release(d.token).unwrap();
            }
        }
        if got == 0 {
            sleep(Duration::from_millis(5));
        }
    }
    out
}

fn errno(r: rvnic::Result<impl std::fmt::Debug>) -> i32 {
    match r {
        Err(rvnic::Error::Io(e)) => e.raw_os_error().unwrap(),
        other => panic!("expected an errno, got {:?}", other),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Rattan vNIC token test ===\n");

    let mut dev0 = RvnicDevice::open()?;
    let mut dev1 = RvnicDevice::open()?;
    let umem = UmemBuilder::new()
        .chunk_size(CHUNK_SIZE as u32)
        .headroom(0)
        .num_chunks(NUM_CHUNKS)
        .build()?;
    dev0.register_umem(umem)?;
    dev1.share_umem(&dev0)?;
    let (mut fill0, mut drop0, mut rx0, _tx0) = dev0.register_rings_for_queue(0, Rings::new()?)?;
    let (mut fill1, mut drop1, mut rx1, mut tx1) =
        dev1.register_rings_for_queue(0, Rings::new()?)?;

    let addrs: Vec<u64> = (0..DEV0_FILL as u64).map(|i| i * CHUNK_SIZE).collect();
    assert_eq!(fill0.produce(&addrs), addrs.len());

    dev0.start(-1)?;
    dev1.start(-1)?;
    let if0 = dev0.device_name()?;
    let if1 = dev1.device_name()?;
    setup_iface(&if0, "10.9.0.1/24", "10.9.0.2");
    setup_iface(&if1, "10.9.1.1/24", "10.9.1.2");
    sleep(Duration::from_millis(100));

    // 1. Encoding
    for seq in 0..5 {
        send("10.9.0.1", "10.9.0.2", seq);
    }
    let pkts = recv(&dev0, &mut rx0, 5);
    for d in &pkts {
        assert_eq!(
            d.token as u32 as u64,
            d.addr / CHUNK_SIZE,
            "token index != chunk index"
        );
        assert_ne!(d.token >> 32, 0, "generation 0 issued");
    }
    println!("1. tokens are gen<<32 | chunk index: ok");
    let [t1, t2, t3, t4, t5] = [0, 1, 2, 3, 4].map(|i| pkts[i]);

    // 2. CLONE
    assert!(errno(dev0.clone_token(t1.token, t2.addr, 0)) == libc::EBUSY);
    assert!(errno(dev0.clone_token(t1.token, CLONE_CHUNK + 1, 0)) == libc::EINVAL);
    assert!(errno(dev0.clone_token(t1.token, CLONE_CHUNK, 7)) == libc::EINVAL);
    assert!(errno(dev0.clone_token(t1.token + (1 << 32), CLONE_CHUNK, 0)) == libc::ENOENT);
    let c1 = dev0.clone_token(t1.token, CLONE_CHUNK, 0)?;
    assert_eq!(c1 as u32 as u64, CLONE_CHUNK / CHUNK_SIZE);
    assert_eq!(payload(&dev0, c1)?, payload(&dev0, t1.token)?);
    // The token also resolves through the device sharing the UMEM
    assert_eq!(payload(&dev1, c1)?, payload(&dev0, t1.token)?);
    println!("2. CLONE: EBUSY/EINVAL/ENOENT on bad input, copy bound to the given chunk: ok");

    // 3. RELEASE, then everything with the stale token misses
    dev0.release(t2.token)?;
    assert!(errno(dev0.release(t2.token)) == libc::ENOENT);
    assert!(payload(&dev0, t2.token).is_err());
    assert!(errno(dev0.clone_token(t2.token, CLONE_CHUNK + CHUNK_SIZE, 0)) == libc::ENOENT);
    println!("3. released token is stale for RELEASE/READ_PAYLOAD/CLONE: ok");

    // 4. A live chunk on FILL: the xmit must be refused, the live token untouched.
    //    Runs before any miss below can complete stray chunks into rattan1's COMP,
    //    which xmit would pop ahead of FILL.
    let before = payload(&dev0, t5.token)?;
    let dropped = stat(&if1, "tx_dropped");
    assert_eq!(fill1.produce(&[t5.addr]), 1);
    send("10.9.1.1", "10.9.1.2", 200);
    sleep(Duration::from_millis(100));
    assert_eq!(
        stat(&if1, "tx_dropped") - dropped,
        1,
        "xmit onto a live chunk not refused"
    );
    assert_eq!(payload(&dev0, t5.token)?, before, "live token clobbered");
    let mut descs = [RattanDesc::default(); 4];
    assert_eq!(
        rx1.consume(&mut descs),
        0,
        "packet published on a live chunk"
    );
    println!("4. live chunk on FILL refused, live token intact: ok");

    // 4b. Addresses past the UMEM, including ones where addr + chunk_size
    //     wraps around u64, must be rejected rather than become a slot index.
    //     rattan1's COMP is still empty, so xmit takes them from FILL.
    let wrap = 0u64.wrapping_sub(CHUNK_SIZE);
    let end = CHUNK_SIZE * NUM_CHUNKS as u64;
    for bad in [wrap, end] {
        assert_eq!(errno(dev0.clone_token(t1.token, bad, 0)), libc::EINVAL);
    }
    let dropped = stat(&if1, "tx_dropped");
    assert_eq!(fill1.produce(&[wrap, end]), 2);
    send("10.9.1.1", "10.9.1.2", 202);
    send("10.9.1.1", "10.9.1.2", 203);
    sleep(Duration::from_millis(100));
    assert_eq!(
        stat(&if1, "tx_dropped") - dropped,
        2,
        "out-of-range chunk not refused"
    );
    assert_eq!(
        rx1.consume(&mut descs),
        0,
        "packet published on an out-of-range chunk"
    );
    println!("4b. out-of-range and wrapping chunk addresses refused: ok");

    // 5. Forward t1 and its clone through rattan1, then replay t1, and put t3 on TX twice
    let base = delivered(&if1);
    assert_eq!(
        tx1.produce(&[
            t1,
            RattanDesc {
                token: c1,
                addr: CLONE_CHUNK,
                ..t1
            }
        ]),
        2
    );
    dev1.kick_rx_queue(0)?;
    sleep(Duration::from_millis(100));
    assert_eq!(delivered(&if1) - base, 2, "forwarded packets not delivered");
    assert!(
        payload(&dev0, t1.token).is_err(),
        "forwarded token still live"
    );

    let base = delivered(&if1);
    assert_eq!(tx1.produce(&[t1, t3, t3]), 3);
    dev1.kick_rx_queue(0)?;
    sleep(Duration::from_millis(100));
    assert_eq!(
        delivered(&if1) - base,
        1,
        "replayed or duplicated token delivered"
    );
    println!("5. forward ok; replayed token and duplicate TX entry miss: ok");

    // 6. The same token on DROP and TX: exactly one of them takes it
    let base = delivered(&if1);
    assert_eq!(drop0.produce(&[t4.token]), 1);
    assert_eq!(tx1.produce(&[t4]), 1);
    dev1.kick_rx_queue(0)?;
    send("10.9.0.1", "10.9.0.2", 100); // an xmit on rattan0 drains its DROP ring
    sleep(Duration::from_millis(100));
    let fwd = delivered(&if1) - base;
    assert!(fwd <= 1);
    assert!(
        payload(&dev0, t4.token).is_err(),
        "token survived both DROP and TX"
    );
    let extra = recv(&dev0, &mut rx0, 1);
    println!(
        "6. DROP + TX of one token: taken once (forwarded={}): ok",
        fwd
    );

    // 7. Close rattan0 while three of its tokens are pending. They stay usable
    //    through rattan1, which shares the UMEM. Finish them via the RELEASE
    //    ioctl, the DROP ring (softirq) and finally NAPI (softirq), which drops
    //    the last reference on rattan0's rings: the cleanup, which sleeps,
    //    must be deferred rather than run there.
    send("10.9.0.1", "10.9.0.2", 300);
    let last = recv(&dev0, &mut rx0, 1)[0];
    drop((fill0, drop0, rx0));
    drop(dev0);
    assert_eq!(payload(&dev1, t5.token)?, before);
    dev1.release(t5.token)?;

    assert_eq!(drop1.produce(&[extra[0].token]), 1);
    send("10.9.1.1", "10.9.1.2", 301); // an xmit on rattan1 drains its DROP ring
    sleep(Duration::from_millis(100));
    assert!(
        payload(&dev1, extra[0].token).is_err(),
        "DROP ring did not release"
    );

    let base = delivered(&if1);
    assert_eq!(tx1.produce(&[last]), 1);
    dev1.kick_rx_queue(0)?;
    sleep(Duration::from_millis(100));
    assert_eq!(delivered(&if1) - base, 1, "orphaned token not forwarded");
    println!("7. tokens outlive their device; last one freed by NAPI: ok");

    // 8. Close the rest
    drop((fill1, drop1, rx1, tx1));
    drop(dev1);
    println!("8. closed (dmesg must show no BUG/WARN)");

    println!("\n=== Test Passed ===");
    Ok(())
}
