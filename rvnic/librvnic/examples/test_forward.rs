//! Test packet forwarding between two vNICs with iperf
//!
//! This test creates two vNIC devices sharing UMEM and forwards packets between them.
//! Each vNIC is placed in a separate network namespace to simulate a real deployment.
//!
//! Topology:
//!   [ns_client]                                    [ns_server]
//!   iperf3 -c 10.0.0.1 --> rattan0 --> [forward] --> rattan1 --> iperf3 -s
//!   10.0.0.2                10.0.0.1                  10.0.0.1    (binds 0.0.0.0)
//!
//! This example uses the queue-0 convenience API and therefore assumes the
//! kernel module is loaded with `num_queues=1`.
//!
//! If you want to test multiqueue forwarding, use `test_forward_multiqueue.rs`
//! instead and load the kernel module with a matching `num_queues` value.
//!
//! Run this with: sudo ./target/debug/examples/test_forward
//!
//! Prerequisites:
//!   1. Build and load the kernel module in single-queue mode:
//!      cd rvnic/kernel && make && sudo insmod rattan_vnic.ko num_queues=1
//!   2. Install iperf3: sudo apt-get install iperf3

use rvnic::sys::RattanDesc;
use rvnic::{
    // CompRing, FillRing,
    RATTAN_RING_SIZE,
    Rings,
    RvnicDevice,
    RxRing,
    TxRing,
    UmemBuilder,
};
use std::os::unix::io::AsRawFd;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

// Batch size for ring operations
const BATCH_SIZE: usize = 256;

// Initial UMEM chunk split between the two devices.
//
// Chunks are owned by the device/queue that originally supplied them and are
// recycled back to that same device/queue. Because the TCP data direction and
// ACK direction are often asymmetric, keep this as a tunable constant so the
// initial pool sizing can be adjusted during performance testing.
//
// Device 0 gets DEV0_CHUNK_SHARE_PERCENT percent of the chunks.
// Device 1 gets the remainder.
const DEV0_CHUNK_SHARE_PERCENT: u32 = 50;

// CPU cores for pinning
//
// Topology (4-core system):
//   CPU 0: iperf client + NAPI for rattan0 (client-side packet injection)
//   CPU 1: forwarder thread (ring management only)
//   CPU 2: iperf server + NAPI for rattan1 (server-side packet injection)
//   CPU 3: spare
//
// This distributes work so the forwarder only does ring operations while
// packet injection (softirq) runs on the same CPU as the application.
const CPU_FORWARDER: u32 = 1;
const CPU_IPERF_SERVER: u32 = 2;
const CPU_IPERF_CLIENT: u32 = 0;
// NAPI CPU for each device (-1 = use caller's CPU at kick_rx time)
// Spread NAPI to client/server CPUs to parallelize with forwarder
const CPU_NAPI_DEV0: i32 = 0; // Client CPU (parallel with forwarder)
const CPU_NAPI_DEV1: i32 = 2; // Server CPU (parallel with forwarder)

/// RAII guard for network namespace cleanup
struct NetnsGuard {
    names: Vec<&'static str>,
}

impl NetnsGuard {
    fn new() -> Self {
        Self { names: Vec::new() }
    }

    fn add(&mut self, name: &'static str) -> std::io::Result<()> {
        // Try to delete first in case it exists from a previous run
        let _ = Command::new("ip").args(["netns", "delete", name]).status();
        let status = Command::new("ip").args(["netns", "add", name]).status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "Failed to create netns {}",
                name
            )));
        }
        self.names.push(name);
        Ok(())
    }
}

impl Drop for NetnsGuard {
    fn drop(&mut self) {
        for name in &self.names {
            let _ = Command::new("ip").args(["netns", "delete", name]).status();
        }
    }
}

/// Direct transfer from COMP to FILL ring (single batch, zero-copy recycling)
// #[inline]
// fn recycle_chunks(comp: &mut CompRing, fill: &mut FillRing, buf: &mut [u64]) -> usize {
//     let n = comp.consume(buf);
//     if n > 0 { fill.produce(&buf[..n]) } else { 0 }
// }

/// Move interface to network namespace
fn move_to_netns(iface: &str, netns: &str) -> std::io::Result<()> {
    let status = Command::new("ip")
        .args(["link", "set", iface, "netns", netns])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to move {} to netns {}",
            iface, netns
        )));
    }
    Ok(())
}

/// Configure interface inside a namespace
fn configure_interface_in_netns(netns: &str, iface: &str, ip: &str) -> std::io::Result<()> {
    let status = Command::new("ip")
        .args([
            "netns", "exec", netns, "ip", "addr", "add", ip, "dev", iface,
        ])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to add IP {} to {} in {}",
            ip, iface, netns
        )));
    }

    let status = Command::new("ip")
        .args(["netns", "exec", netns, "ip", "link", "set", iface, "up"])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to bring up {} in {}",
            iface, netns
        )));
    }

    let _ = Command::new("ip")
        .args(["netns", "exec", netns, "ip", "link", "set", "lo", "up"])
        .status();

    Ok(())
}

/// Start iperf3 server in a namespace, pinned to a CPU
fn start_iperf_server_in_netns(netns: &str, cpu: u32) -> std::io::Result<Child> {
    Command::new("taskset")
        .args([
            "-c",
            &cpu.to_string(),
            "ip",
            "netns",
            "exec",
            netns,
            "iperf3",
            "-s",
            "-1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Run iperf3 client in a namespace, pinned to a CPU
fn run_iperf_client_in_netns(
    netns: &str,
    server_addr: &str,
    duration: u32,
    parallel: u32,
    cpu: u32,
) -> std::io::Result<String> {
    let cpu_str = cpu.to_string();
    let duration_str = duration.to_string();
    let parallel_str = parallel.to_string();

    let mut args = vec![
        "-c",
        &cpu_str,
        "ip",
        "netns",
        "exec",
        netns,
        "iperf3",
        "-c",
        server_addr,
        "-t",
        &duration_str,
        "-i",
        "1",
        "--forceflush",
    ];

    if parallel > 1 {
        args.extend(["-P", &parallel_str]);
    }

    let output = Command::new("taskset").args(&args).output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "iperf3 client failed: {}",
            stderr
        )));
    }

    Ok(stdout.to_string())
}

/// Set CPU affinity for the current thread
fn set_cpu_affinity(cpu: u32) -> std::io::Result<()> {
    unsafe {
        let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut cpuset);
        libc::CPU_SET(cpu as usize, &mut cpuset);

        let ret = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset);
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Forward packets from one RX ring to another TX ring (single batch)
#[inline]
fn forward_batch(rx: &mut RxRing, tx: &mut TxRing, descs: &mut [RattanDesc]) -> usize {
    let received = rx.consume(descs);
    if received > 0 {
        tx.produce(&descs[..received])
    } else {
        0
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Rattan vNIC Forwarding Performance Test ===\n");

    // Step 1: Create network namespaces (with RAII cleanup)
    println!("1. Creating network namespaces...");
    let mut netns_guard = NetnsGuard::new();
    netns_guard.add("ns_client")?;
    netns_guard.add("ns_server")?;
    println!("   Created ns_client and ns_server");

    // Step 2: Setup devices with shared UMEM
    println!("\n2. Setting up vNIC devices...");

    let mut dev0 = RvnicDevice::open()?;
    let device_name_0 = dev0.device_name()?;
    println!("   {} opened (fd={})", device_name_0, dev0.fd());

    let mut dev1 = RvnicDevice::open()?;
    let device_name_1 = dev1.device_name()?;
    println!("   {} opened (fd={})", device_name_1, dev1.fd());

    // Allocate shared UMEM - use 2x ring size total to allow buffering across
    // both devices. Chunks are ingress-owned and are recycled back to the
    // device that originally supplied them, so the initial split between the
    // two devices is configurable below.
    let chunks_per_device = RATTAN_RING_SIZE;
    let total_chunks = chunks_per_device * 2;

    let umem = UmemBuilder::new()
        .chunk_size(256)
        .headroom(64)
        .num_chunks(total_chunks)
        .build()?;

    println!(
        "   UMEM allocated: {} chunks x {} bytes ({} chunks/device worth total capacity)",
        umem.num_chunks(),
        umem.chunk_size(),
        chunks_per_device
    );

    let chunk_size = umem.chunk_size() as u64;
    let num_chunks = umem.num_chunks();

    dev0.register_umem(umem)?;
    dev1.share_umem(&dev0)?;
    println!("   UMEM registered and shared");

    // Setup rings and get ring handles
    let rings0 = Rings::new()?;
    let (mut fill0, mut _drop0, mut rx0, mut tx0) = dev0.register_rings_for_queue(0, rings0)?;

    let rings1 = Rings::new()?;
    let (mut fill1, mut _drop1, mut rx1, mut tx1) = dev1.register_rings_for_queue(0, rings1)?;
    println!("   Rings registered");

    // Distribute chunks between the two devices. Chunks are ingress-owned and
    // return to the same device/queue that originally supplied them, so this
    // initial split controls the steady-state buffer budget available to each
    // direction.
    let mut buf = [0u64; BATCH_SIZE];
    let mut dist0 = 0usize;
    let mut dist1 = 0usize;
    let dev0_chunks = ((num_chunks * DEV0_CHUNK_SHARE_PERCENT) / 100) as usize;
    let dev1_start = dev0_chunks;
    let dev1_chunks = num_chunks as usize - dev0_chunks;

    // Fill ring0 with the device 0 share
    for chunk_start in (0..dev0_chunks).step_by(BATCH_SIZE) {
        let chunk_end = (chunk_start + BATCH_SIZE).min(dev0_chunks);
        let batch_size = chunk_end - chunk_start;

        for (i, addr) in buf.iter_mut().enumerate().take(batch_size) {
            *addr = ((chunk_start + i) as u64) * chunk_size;
        }

        dist0 += fill0.produce(&buf[..batch_size]);
    }

    // Fill ring1 with the remaining share for device 1
    for chunk_start in (dev1_start..(dev1_start + dev1_chunks)).step_by(BATCH_SIZE) {
        let chunk_end = (chunk_start + BATCH_SIZE).min(dev1_start + dev1_chunks);
        let batch_size = chunk_end - chunk_start;

        for (i, addr) in buf.iter_mut().enumerate().take(batch_size) {
            *addr = ((chunk_start + i) as u64) * chunk_size;
        }

        dist1 += fill1.produce(&buf[..batch_size]);
    }

    println!("   Chunks allocated: {}", num_chunks);
    println!(
        "   FILL rings populated ({} to ring0, {} to ring1, dev0 share={}%)",
        dist0, dist1, DEV0_CHUNK_SHARE_PERCENT
    );

    // Start devices with NAPI CPU configuration
    dev0.start(CPU_NAPI_DEV0)?;
    dev1.start(CPU_NAPI_DEV1)?;
    println!("   Devices started");
    if CPU_NAPI_DEV0 >= 0 {
        println!(
            "   {} NAPI CPU: {} (client-side injection)",
            device_name_0, CPU_NAPI_DEV0
        );
    } else {
        println!("   {} NAPI: use caller's CPU", device_name_0);
    }
    if CPU_NAPI_DEV1 >= 0 {
        println!(
            "   {} NAPI CPU: {} (server-side injection)",
            device_name_1, CPU_NAPI_DEV1
        );
    } else {
        println!("   rattan1 NAPI: use caller's CPU");
    }

    // Step 3: Move interfaces to namespaces and configure
    println!("\n3. Configuring network namespaces...");

    move_to_netns(device_name_0.as_str(), "ns_client")?;
    println!("   Moved {} to ns_client", device_name_0);

    move_to_netns(device_name_1.as_str(), "ns_server")?;
    println!("   Moved {} to ns_server", device_name_1);

    configure_interface_in_netns("ns_client", device_name_0.as_str(), "10.0.0.2/24")?;
    println!("   ns_client: {} = 10.0.0.2/24", device_name_0);

    configure_interface_in_netns("ns_server", device_name_1.as_str(), "10.0.0.1/24")?;
    println!("   ns_server: {} = 10.0.0.1/24", device_name_1);

    // Step 4: Start forwarding loop
    println!("\n4. Starting forwarding loop...");

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let packets_forwarded = Arc::new(AtomicU64::new(0));
    let packets_forwarded_clone = packets_forwarded.clone();

    // Get file descriptors for epoll
    let dev0_fd = dev0.as_raw_fd();
    let dev1_fd = dev1.as_raw_fd();

    // Single-threaded forwarder using epoll, pinned to CPU
    let forward_thread = std::thread::spawn(move || {
        // Pin to dedicated CPU core
        if let Err(e) = set_cpu_affinity(CPU_FORWARDER) {
            eprintln!("Warning: failed to set CPU affinity: {}", e);
        }

        let mut descs = [RattanDesc::default(); BATCH_SIZE];
        // let mut addrs = [0u64; BATCH_SIZE];

        // Create epoll instance
        let epfd = unsafe { libc::epoll_create1(0) };
        if epfd < 0 {
            eprintln!("epoll_create1 failed");
            return (dev0, dev1);
        }

        // Add dev0 to epoll (data.u32 = 0)
        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: 0,
        };
        if unsafe { libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, dev0_fd, &mut ev) } < 0 {
            eprintln!("epoll_ctl add dev0 failed");
            unsafe { libc::close(epfd) };
            return (dev0, dev1);
        }

        // Add dev1 to epoll (data.u32 = 1)
        ev.u64 = 1;
        if unsafe { libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, dev1_fd, &mut ev) } < 0 {
            eprintln!("epoll_ctl add dev1 failed");
            unsafe { libc::close(epfd) };
            return (dev0, dev1);
        }

        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 2];

        while running_clone.load(Ordering::Relaxed) {
            // Wait for events on either device (10ms timeout)
            let nfds = unsafe { libc::epoll_wait(epfd, events.as_mut_ptr(), 2, 10) };

            if nfds < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::Interrupted {
                    eprintln!("epoll_wait failed: {}", err);
                    break;
                }
                continue;
            }

            // Process events - drain RX rings completely
            for i in 0..nfds as usize {
                let dev_idx = events[i].u64;

                if dev_idx == 0 {
                    // dev0 has RX data: forward rx0 → tx1
                    // Drain all available packets (not just one batch)
                    loop {
                        let fwd = forward_batch(&mut rx0, &mut tx1, &mut descs);
                        if fwd > 0 {
                            packets_forwarded_clone.fetch_add(fwd as u64, Ordering::Relaxed);
                            let _ = dev1.kick_rx_queue(0);
                        }
                        if fwd < BATCH_SIZE {
                            break; // Ring drained
                        }
                    }
                } else {
                    // dev1 has RX data: forward rx1 → tx0
                    // Drain all available packets (not just one batch)
                    loop {
                        let fwd = forward_batch(&mut rx1, &mut tx0, &mut descs);
                        if fwd > 0 {
                            packets_forwarded_clone.fetch_add(fwd as u64, Ordering::Relaxed);
                            let _ = dev0.kick_rx_queue(0);
                        }
                        if fwd < BATCH_SIZE {
                            break; // Ring drained
                        }
                    }
                }
            }

            // Always recycle chunks back to the device that originally supplied
            // them. With current kernel completion routing, chunks are
            // ingress-owned and completions return to the originating
            // device/queue, not the forwarding destination.
            // loop {
            //     let r0 = recycle_chunks(&mut comp0, &mut fill0, &mut addrs);
            //     let r1 = recycle_chunks(&mut comp1, &mut fill1, &mut addrs);
            //     if r0 == 0 && r1 == 0 {
            //         break;
            //     }
            // }
        }

        unsafe { libc::close(epfd) };
        (dev0, dev1)
    });

    println!(
        "   Forwarding loop started (single thread, epoll, CPU {})",
        CPU_FORWARDER
    );

    // Step 5: Run iperf test
    println!("\n5. Running iperf3 performance test...");
    println!(
        "   Starting iperf3 server in ns_server (CPU {})...",
        CPU_IPERF_SERVER
    );
    let mut server = start_iperf_server_in_netns("ns_server", CPU_IPERF_SERVER)?;

    std::thread::sleep(Duration::from_millis(500));

    let parallel_streams = 4;
    println!(
        "   Running iperf3 client from ns_client (10 seconds, {} parallel streams, CPU {})...\n",
        parallel_streams, CPU_IPERF_CLIENT
    );
    let start = Instant::now();

    match run_iperf_client_in_netns(
        "ns_client",
        "10.0.0.1",
        10,
        parallel_streams,
        CPU_IPERF_CLIENT,
    ) {
        Ok(output) => println!("{}", output),
        Err(e) => eprintln!("   iperf3 client error: {}", e),
    }

    let elapsed = start.elapsed();
    println!("\n   Test completed in {:.2}s", elapsed.as_secs_f64());

    let _ = server.wait();

    // Step 6: Print statistics
    println!("\n6. Statistics:");
    let total_forwarded = packets_forwarded.load(Ordering::Relaxed);
    println!("   Packets forwarded: {}", total_forwarded);

    // Stop forwarding
    println!("\n7. Cleaning up...");
    running.store(false, Ordering::Relaxed);

    let (_dev0, _dev1) = forward_thread.join().unwrap();

    // netns_guard will clean up namespaces on drop
    drop(netns_guard);
    println!("   Namespaces deleted");

    println!("   Done");
    println!("\n=== Test Complete ===");

    Ok(())
}
