//! Test multiqueue packet forwarding between two vNICs with shared UMEM.
//!
//! This example demonstrates the ergonomic multiqueue-facing `librvnic` API:
//!
//! - one shared UMEM
//! - one ring set per queue
//! - `QueueConfig` / `register_queues(...)`
//! - `QueueRings` bundles for per-queue operation
//! - queue-specific `kick_rx_queue(...)`
//! - one queue-worker thread per queue
//!
//! Topology:
//!   [ns_client]                                                [ns_server]
//!   iperf3 -c 10.0.0.1 --> rattan0[q0..q3] --> [forward] --> rattan1[q0..q3] --> iperf3 -s
//!
//! Chunk ownership model:
//! - chunks are owned by the ingress device/queue that originally supplied them
//! - completions return to that same queue's COMP ring
//! - userspace should recycle `comp -> fill` on the same queue
//!
//! Notes:
//! - This example assumes the kernel module is loaded with `num_queues=4`.
//! - Queue count is not yet discoverable from userspace, so it is configured here
//!   explicitly via `NUM_QUEUES`.
//! - The initial UMEM chunk split between device 0 and device 1 is controlled by
//!   `DEV0_CHUNK_SHARE_PERCENT` below so it can be tuned later.
//! - Queue workers use `napi_cpu = -1`, so each `kick_rx_queue(...)` schedules
//!   NAPI on the calling queue worker's CPU.
//! - This is a manual example. Do not run it automatically in tests.
//!
//! Example module load:
//!   sudo insmod rattan_vnic.ko num_queues=4
//!
//! Run manually with:
//!   sudo cargo run --example test_forward_multiqueue
//!
//! Prerequisites:
//!   1. Build and load the kernel module with `num_queues=4`
//!   2. Install iperf3: sudo apt-get install iperf3
//!   3. Ensure `/dev/rattan-vnic` exists

use rvnic::sys::{RattanDesc, ioctl_kick_rx_q};
use rvnic::{
    QueueConfig, QueueRings, RATTAN_DEFAULT_CHUNK_SIZE, RATTAN_RING_SIZE, Rings, RvnicDevice,
    UmemBuilder,
};

use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const NUM_QUEUES: u32 = 4;
const BATCH_SIZE: usize = 256;

// CPU layout for a 16-core machine:
//   CPU 0-3:  queue workers for q0..q3 (also drive NAPI via kick_rx_queue)
//   CPU 4-9:  iperf client CPU set
//   CPU 10-15: iperf server CPU set
const QUEUE_WORKER_CPUS: [u32; NUM_QUEUES as usize] = [0, 1, 2, 3];
const CPUSET_IPERF_CLIENT: &str = "4-9";
const CPUSET_IPERF_SERVER: &str = "10-15";
const NAPI_CPU: i32 = -1;

struct NetnsGuard {
    names: Vec<&'static str>,
}

impl NetnsGuard {
    fn new() -> Self {
        Self { names: Vec::new() }
    }

    fn add(&mut self, name: &'static str) -> std::io::Result<()> {
        let _ = Command::new("ip").args(["netns", "delete", name]).status();
        let status = Command::new("ip").args(["netns", "add", name]).status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "failed to create netns {}",
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

fn move_to_netns(iface: &str, netns: &str) -> std::io::Result<()> {
    let status = Command::new("ip")
        .args(["link", "set", iface, "netns", netns])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "failed to move {} to {}",
            iface, netns
        )));
    }
    Ok(())
}

fn configure_interface_in_netns(netns: &str, iface: &str, ip: &str) -> std::io::Result<()> {
    let status = Command::new("ip")
        .args([
            "netns", "exec", netns, "ip", "addr", "add", ip, "dev", iface,
        ])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "failed to add IP {} to {} in {}",
            ip, iface, netns
        )));
    }

    let status = Command::new("ip")
        .args(["netns", "exec", netns, "ip", "link", "set", iface, "up"])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "failed to bring up {} in {}",
            iface, netns
        )));
    }

    let _ = Command::new("ip")
        .args(["netns", "exec", netns, "ip", "link", "set", "lo", "up"])
        .status();

    Ok(())
}

fn start_iperf_server_in_netns(netns: &str, cpuset: &str) -> std::io::Result<Child> {
    Command::new("taskset")
        .args([
            "-c", cpuset, "ip", "netns", "exec", netns, "iperf3", "-s", "-1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn run_iperf_client_in_netns(
    netns: &str,
    server_addr: &str,
    duration: u32,
    parallel: u32,
    cpuset: &str,
) -> std::io::Result<String> {
    let duration_str = duration.to_string();
    let parallel_str = parallel.to_string();

    let mut args = vec![
        "-c",
        cpuset,
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

    if parallel == 4 {
        args.extend(["--cport", "3333"]);
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

#[inline]
fn forward_batch(
    rx: &mut rvnic::RxRing,
    tx: &mut rvnic::TxRing,
    descs: &mut [RattanDesc],
) -> usize {
    let received = rx.consume(descs);
    if received > 0 {
        tx.produce(&descs[..received])
    } else {
        0
    }
}

// #[inline]
// fn recycle_chunks(
//     comp: &mut rvnic::CompRing,
//     fill: &mut rvnic::FillRing,
//     buf: &mut [u64],
// ) -> usize {
//     let n = comp.consume(buf);
//     if n > 0 { fill.produce(&buf[..n]) } else { 0 }
// }

fn build_queue_configs(num_queues: u32) -> Result<Vec<QueueConfig>, Box<dyn std::error::Error>> {
    let mut queues = Vec::with_capacity(num_queues as usize);
    for queue_id in 0..num_queues {
        queues.push(QueueConfig {
            queue_id,
            rings: Rings::new()?,
        });
    }
    Ok(queues)
}

fn prefill_device_queues(
    queues: &mut [QueueRings],
    start_chunk: u32,
    num_chunks: u32,
    chunk_size: u64,
) -> usize {
    let mut total = 0usize;
    let mut addrs = [0u64; BATCH_SIZE];

    let end_chunk = start_chunk + num_chunks;

    let mut queue_index = (0..queues.len()).cycle();

    for batch_chunk_start in (start_chunk..end_chunk).step_by(BATCH_SIZE) {
        let batch_chunk_end = (batch_chunk_start + BATCH_SIZE as u32).min(end_chunk);
        let batch_size = (batch_chunk_end - batch_chunk_start) as usize;

        for (i, addr) in addrs.iter_mut().enumerate().take(batch_size) {
            *addr = (batch_chunk_start + i as u32) as u64 * chunk_size;
        }

        let queue = queue_index.next().unwrap();

        let produced = queues[queue].fill.produce(&addrs[..batch_size]);
        total += produced;
    }

    assert_eq!(num_chunks, total as u32);

    total
}

fn drain_one_direction(
    src: &mut QueueRings,
    dst: &mut QueueRings,
    dst_fd: i32,
    descs: &mut [RattanDesc],
    packets_forwarded: &AtomicU64,
) -> usize {
    let mut total = 0usize;

    loop {
        let forwarded = forward_batch(&mut src.rx, &mut dst.tx, descs);
        if forwarded > 0 {
            packets_forwarded.fetch_add(forwarded as u64, Ordering::Relaxed);
            unsafe {
                let _ = ioctl_kick_rx_q(dst_fd, dst.queue_id);
            }
            total += forwarded;
        }
        if forwarded < BATCH_SIZE {
            break;
        }
    }

    total
}

// fn drain_one_queue_recycle(queue: &mut QueueRings, addrs: &mut [u64]) -> usize {
//     let mut recycled = 0usize;

//     loop {
//         let n = recycle_chunks(&mut queue.comp, &mut queue.fill, addrs);
//         recycled += n;
//         if n < addrs.len() {
//             break;
//         }
//     }

//     recycled
// }

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Rattan vNIC Multiqueue Forwarding Test ===\n");
    println!("Configured queue count: {}", NUM_QUEUES);
    println!(
        "This example assumes the kernel module was loaded with num_queues={}.\n",
        NUM_QUEUES
    );
    println!(
        "CPU layout: queue workers={:?}, client cpuset={}, server cpuset={}\n",
        QUEUE_WORKER_CPUS, CPUSET_IPERF_CLIENT, CPUSET_IPERF_SERVER
    );

    println!("1. Creating network namespaces...");
    let mut netns_guard = NetnsGuard::new();
    netns_guard.add("ns_client")?;
    netns_guard.add("ns_server")?;
    println!("   Created ns_client and ns_server");

    println!("\n2. Setting up multiqueue vNIC devices...");

    let mut dev0 = RvnicDevice::open()?;
    println!("   rattan0 opened (fd={})", dev0.fd());

    let mut dev1 = RvnicDevice::open()?;
    println!("   rattan1 opened (fd={})", dev1.fd());

    let chunks_per_queue = RATTAN_RING_SIZE;

    let chunks_per_dev = chunks_per_queue * NUM_QUEUES;
    let total_chunks = chunks_per_dev * 2;

    let umem = UmemBuilder::new()
        .chunk_size(RATTAN_DEFAULT_CHUNK_SIZE)
        .headroom(128)
        .num_chunks(total_chunks)
        .build()?;

    println!(
        "   UMEM allocated: {} chunks x {} bytes",
        umem.num_chunks(),
        umem.chunk_size()
    );

    let chunk_size = umem.chunk_size() as u64;
    let num_chunks = umem.num_chunks();

    dev0.register_umem(umem)?;
    dev1.share_umem(&dev0)?;
    println!("   UMEM registered and shared");

    let dev0_configs = build_queue_configs(NUM_QUEUES)?;
    let dev1_configs = build_queue_configs(NUM_QUEUES)?;

    let mut dev0_queues = dev0.register_queues(dev0_configs)?;
    let mut dev1_queues = dev1.register_queues(dev1_configs)?;
    println!("   Registered {} queues on each device", NUM_QUEUES);

    let dev0_chunks = chunks_per_dev;
    let dev1_chunks = num_chunks - dev0_chunks;
    let dev0_fill = prefill_device_queues(&mut dev0_queues, 0, dev0_chunks, chunk_size);
    let dev1_fill = prefill_device_queues(&mut dev1_queues, dev0_chunks, dev1_chunks, chunk_size);

    println!(
        "   Pre-filled FILL rings: {} chunks to dev0, {} chunks to dev1 ",
        dev0_fill, dev1_fill,
    );

    dev0.start(NAPI_CPU)?;
    dev1.start(NAPI_CPU)?;
    println!("   Devices started");
    println!("   dev0 NAPI CPU: {}", NAPI_CPU);
    println!("   dev1 NAPI CPU: {}", NAPI_CPU);

    println!("\n3. Configuring network namespaces...");

    move_to_netns("rattan0", "ns_client")?;
    println!("   Moved rattan0 to ns_client");

    move_to_netns("rattan1", "ns_server")?;
    println!("   Moved rattan1 to ns_server");

    configure_interface_in_netns("ns_client", "rattan0", "10.0.0.2/24")?;
    println!("   ns_client: rattan0 = 10.0.0.2/24");

    configure_interface_in_netns("ns_server", "rattan1", "10.0.0.1/24")?;
    println!("   ns_server: rattan1 = 10.0.0.1/24");

    println!("\n4. Starting multiqueue forwarding loop...");

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    let packets_forwarded_cnts: Vec<_> = (0..NUM_QUEUES)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();

    let dev0_fd = dev0.fd();
    let dev1_fd = dev1.fd();

    let mut dev0_workers: Vec<Option<QueueRings>> = dev0_queues.into_iter().map(Some).collect();
    let mut dev1_workers: Vec<Option<QueueRings>> = dev1_queues.into_iter().map(Some).collect();
    let mut forward_threads = Vec::with_capacity(NUM_QUEUES as usize);

    for (queue_idx, cpu) in QUEUE_WORKER_CPUS.iter().copied().enumerate() {
        let mut dev0_queue = dev0_workers[queue_idx].take().unwrap();
        let mut dev1_queue = dev1_workers[queue_idx].take().unwrap();
        let running = Arc::clone(&running_clone);
        let packets_forwarded = packets_forwarded_cnts[queue_idx].clone();
        let dev0_fd = dev0_fd;
        let dev1_fd = dev1_fd;

        let handle = std::thread::spawn(move || {
            if let Err(e) = set_cpu_affinity(cpu) {
                eprintln!(
                    "warning: failed to set CPU affinity for queue {}: {}",
                    queue_idx, e
                );
            }

            let mut descs = [RattanDesc::default(); BATCH_SIZE];
            let mut _addrs = [0u64; BATCH_SIZE];

            while running.load(Ordering::Relaxed) {
                let mut progressed = 0usize;

                progressed += drain_one_direction(
                    &mut dev0_queue,
                    &mut dev1_queue,
                    dev1_fd,
                    &mut descs,
                    &packets_forwarded,
                );

                progressed += drain_one_direction(
                    &mut dev1_queue,
                    &mut dev0_queue,
                    dev0_fd,
                    &mut descs,
                    &packets_forwarded,
                );

                // progressed += drain_one_queue_recycle(&mut dev0_queue, &mut addrs);
                // progressed += drain_one_queue_recycle(&mut dev1_queue, &mut addrs);

                if progressed == 0 {
                    std::thread::yield_now();
                }
            }

            (dev0_queue, dev1_queue)
        });

        forward_threads.push(handle);
    }

    println!(
        "   Forwarding started ({} queue-worker threads on CPUs {:?})",
        NUM_QUEUES, QUEUE_WORKER_CPUS
    );

    println!("\n5. Running iperf3 performance test...");
    println!(
        "   Starting iperf3 server in ns_server (CPU set {})...",
        CPUSET_IPERF_SERVER
    );
    let mut server = start_iperf_server_in_netns("ns_server", CPUSET_IPERF_SERVER)?;

    std::thread::sleep(Duration::from_millis(500));

    let parallel_streams = 4;
    println!(
        "   Running iperf3 client from ns_client (10 seconds, {} parallel streams, CPU set {})...\n",
        parallel_streams, CPUSET_IPERF_CLIENT
    );
    let start = Instant::now();

    match run_iperf_client_in_netns(
        "ns_client",
        "10.0.0.1",
        10,
        parallel_streams,
        CPUSET_IPERF_CLIENT,
    ) {
        Ok(output) => println!("{}", output),
        Err(e) => eprintln!("   iperf3 client error: {}", e),
    }

    let elapsed = start.elapsed();
    println!("\n   Test completed in {:.2}s", elapsed.as_secs_f64());

    let _ = server.wait();

    println!("\n6. Statistics:");

    for queue_index in 0..NUM_QUEUES {
        let packets_forwarded = packets_forwarded_cnts
            .get(queue_index as usize)
            .unwrap()
            .clone();
        let total_forwarded = packets_forwarded.load(Ordering::Relaxed);
        println!(
            "   Packets forwarded on queue {}: {}",
            queue_index, total_forwarded
        );
    }

    println!("\n7. Cleaning up...");
    running.store(false, Ordering::Relaxed);

    for handle in forward_threads {
        let (_dev0_queue, _dev1_queue) = handle.join().unwrap();
    }

    drop(netns_guard);
    println!("   Namespaces deleted");

    println!("   Done");
    println!("\n=== Test Complete ===");

    Ok(())
}
