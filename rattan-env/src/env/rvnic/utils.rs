use rtnetlink::{LinkMessageBuilder, LinkUnspec};
use rvnic::{QueueConfig, QueueRings, Rings};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use tracing::info;

use super::constants::BATCH_SIZE;

/// A thin wrapper around Linux epoll.
///
/// Design goals:
/// - Minimal abstraction over libc epoll API
/// - Keep full control over flags (EPOLLET, etc.)
/// - Localize unsafe usage
/// - Use u64 token to identify fds (like mio::Token)
///
/// This is suitable for high-performance, single-threaded
/// event loops (e.g., AF_XDP, custom network stacks).
pub struct Epoll {
    epfd: RawFd,
}

impl Epoll {
    /// Create a new epoll instance.
    ///
    /// Internally calls epoll_create1.
    pub fn new() -> io::Result<Self> {
        let epfd = unsafe { libc::epoll_create1(0) };
        if epfd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { epfd })
    }

    /// Add a file descriptor to the epoll set.
    ///
    /// # Arguments
    ///
    /// - `fd`: file descriptor to monitor
    /// - `token`: user-defined identifier (stored in epoll_event.u64)
    ///
    /// # Behavior
    ///
    /// - Registers for READABLE events (EPOLLIN)
    /// - Uses EDGE-TRIGGERED mode (EPOLLET)
    ///
    /// # Important (Edge-triggered semantics!)
    ///
    /// Once you receive a readiness event:
    /// - You MUST drain the fd completely (read until EAGAIN)
    /// - Otherwise, you may never receive another event
    ///
    pub fn add(&self, fd: RawFd, token: u64) -> io::Result<()> {
        let mut ev = libc::epoll_event {
            events: (libc::EPOLLIN | libc::EPOLLET) as u32,
            u64: token,
        };

        let ret = unsafe { libc::epoll_ctl(self.epfd, libc::EPOLL_CTL_ADD, fd, &mut ev) };

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Wait for events.
    ///
    /// # Arguments
    ///
    /// - `events`: preallocated buffer
    /// - `timeout_ms`: timeout in milliseconds
    ///     - -1: block indefinitely
    ///     -  0: non-blocking poll
    ///
    /// # Returns
    ///
    /// Number of ready events
    ///
    /// # EINTR handling
    ///
    /// If interrupted by a signal (e.g., SIGWINCH),
    /// this function will automatically retry.
    ///
    pub fn wait(&self, events: &mut [libc::epoll_event], timeout_ms: i32) -> io::Result<usize> {
        loop {
            let nfds = unsafe {
                libc::epoll_wait(
                    self.epfd,
                    events.as_mut_ptr(),
                    events.len() as i32,
                    timeout_ms,
                )
            };

            if nfds < 0 {
                let err = io::Error::last_os_error();

                // Retry on EINTR (signal interruption)
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }

                return Err(err);
            }

            return Ok(nfds as usize);
        }
    }
}

impl Drop for Epoll {
    /// Ensure epoll fd is closed when dropped.
    fn drop(&mut self) {
        unsafe {
            libc::close(self.epfd);
        }
    }
}

// Utility functions

/// Move a network interface into a specific network namespace using native netlink
pub fn move_to_netns(iface: &str, netns: &str) -> std::io::Result<()> {
    if netns.is_empty() {
        return Ok(());
    }

    // Initialize a current-thread Tokio runtime to execute netlink commands
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        // Open the target network namespace file to get its file descriptor
        let ns_path = format!("/var/run/netns/{}", netns);
        let fd = File::open(&ns_path).map_err(|e| {
            std::io::Error::other(format!("Failed to open netns file {}: {}", ns_path, e))
        })?;

        let (connection, handle, _) =
            rtnetlink::new_connection().map_err(|e| std::io::Error::other(e.to_string()))?;
        tokio::spawn(connection);

        // Retrieve the interface index (ifindex) of the link in the current namespace
        let mut links = handle.link().get().match_name(iface.to_string()).execute();
        let link = links
            .try_next()
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .ok_or_else(|| {
                std::io::Error::other(format!("Interface {} not found in current netns", iface))
            })?;
        let ifindex = link.header.index;

        let msg = LinkMessageBuilder::<LinkUnspec>::new()
            .index(ifindex)
            .setns_by_fd(fd.as_raw_fd())
            .build();

        // Move the interface into the target network namespace using its file descriptor
        handle.link().change(msg).execute().await.map_err(|e| {
            std::io::Error::other(format!(
                "Failed to move interface {} to netns {}: {}",
                iface, netns, e
            ))
        })?;

        Ok::<(), std::io::Error>(())
    })?;

    // (Ensure your logging crate like `log` or `tracing` is imported in the file)
    info!("Moved RVNIC {} into network namespace {}", iface, netns);
    Ok(())
}

pub fn build_queue_configs(num_queues: u32) -> rvnic::Result<Vec<QueueConfig>> {
    let mut queues = Vec::with_capacity(num_queues as usize);
    for queue_id in 0..num_queues {
        queues.push(QueueConfig {
            queue_id,
            rings: Rings::new()?,
        });
    }
    Ok(queues)
}

pub fn prefill_device_queues(
    queues: &mut [QueueRings],
    start_chunk: u32,
    num_chunks: u32,
    chunk_size: u32,
) -> usize {
    let mut total = 0usize;
    let mut addrs = [0u64; BATCH_SIZE];
    let end_chunk = start_chunk + num_chunks;
    let mut queue_index = (0..queues.len()).cycle();

    for batch_chunk_start in (start_chunk..end_chunk).step_by(BATCH_SIZE) {
        let batch_chunk_end = (batch_chunk_start + BATCH_SIZE as u32).min(end_chunk);
        let batch_size = (batch_chunk_end - batch_chunk_start) as usize;
        for (i, addr) in addrs.iter_mut().enumerate().take(batch_size) {
            *addr = (batch_chunk_start as u64 + i as u64) * chunk_size as u64;
        }
        let queue = queue_index.next().unwrap();
        let produced = queues[queue].fill.produce(&addrs[..batch_size]);
        total += produced;
    }

    total
}

use futures::stream::TryStreamExt;
use nix::sched::{setns, CloneFlags};
use std::fs::File;
use std::net::IpAddr;

/// Configure interface inside a specific network namespace using native netlink
pub fn configure_interface_in_netns(
    netns: &str,
    iface: &str,
    ip_addr: IpAddr,
) -> std::io::Result<()> {
    let netns = netns.to_string();
    let iface = iface.to_string();

    let prefix_len = if ip_addr.is_ipv4() { 24 } else { 64 };

    // Spawn a dedicated OS thread because setns changes the network namespace
    // of the calling thread only, preventing namespace pollution of the main thread.
    std::thread::spawn(move || {
        let ns_path = format!("/var/run/netns/{}", netns);
        let fd = File::open(&ns_path).map_err(|e| {
            std::io::Error::other(format!("Failed to open netns file {}: {}", ns_path, e))
        })?;

        setns(&fd, CloneFlags::CLONE_NEWNET)
            .map_err(|e| std::io::Error::other(format!("Failed to setns to {}: {}", netns, e)))?;

        // Initialize a current-thread Tokio runtime inside the target namespace
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        rt.block_on(async {
            let (connection, handle, _) =
                rtnetlink::new_connection().map_err(|e| std::io::Error::other(e.to_string()))?;
            // Drive the netlink socket connection in the background
            tokio::spawn(connection);

            // Retrieve the interface index (ifindex) by its name
            let mut links = handle.link().get().match_name(iface.clone()).execute();
            let link = links
                .try_next()
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "Interface {} not found in netns {}",
                        iface, netns
                    ))
                })?;
            let ifindex = link.header.index;

            // Assign the IP address to the interface
            handle
                .address()
                .add(ifindex, ip_addr, prefix_len)
                .execute()
                .await
                .map_err(|e| {
                    std::io::Error::other(format!("Failed to add IP via netlink: {}", e))
                })?;

            let msg = LinkMessageBuilder::<LinkUnspec>::new()
                .index(ifindex)
                .up()
                .build();

            // Bring up the interface (IFF_UP) using the property handle
            handle.link().change(msg).execute().await.map_err(|e| {
                std::io::Error::other(format!("Failed to bring up {} via netlink: {}", iface, e))
            })?;

            // Bring up the loopback interface as well
            let mut lo_links = handle.link().get().match_name("lo".to_string()).execute();
            if let Ok(Some(lo_link)) = lo_links.try_next().await {
                let msg = LinkMessageBuilder::<LinkUnspec>::new()
                    .index(lo_link.header.index)
                    .up()
                    .build();
                handle.link().change(msg).execute().await.map_err(|e| {
                    std::io::Error::other(format!("Failed to bring lo via netlink: {}", e))
                })?;
            }

            Ok(())
        })
    })
    .join()
    .map_err(|_| std::io::Error::other("Netns configuration thread panicked"))?
}

/// Configure interface in the current network namespace using native netlink
pub fn configure_interface_in_current_netns(iface: &str, ip_addr: IpAddr) -> std::io::Result<()> {
    let iface = iface.to_string();

    let prefix_len = if ip_addr.is_ipv4() { 24 } else { 64 };

    // Directly initialize and block on the runtime since nested runtimes are not a concern
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        let (connection, handle, _) =
            rtnetlink::new_connection().map_err(|e| std::io::Error::other(e.to_string()))?;
        tokio::spawn(connection);

        let mut links = handle.link().get().match_name(iface.clone()).execute();
        let link = links
            .try_next()
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .ok_or_else(|| std::io::Error::other(format!("Interface {} not found", iface)))?;
        let ifindex = link.header.index;

        handle
            .address()
            .add(ifindex, ip_addr, prefix_len)
            .execute()
            .await
            .map_err(|e| std::io::Error::other(format!("Failed to add IP: {}", e)))?;

        let msg = LinkMessageBuilder::<LinkUnspec>::new()
            .index(ifindex)
            .up()
            .build();

        // Bring up the interface (IFF_UP) using the property handle
        handle.link().change(msg).execute().await.map_err(|e| {
            std::io::Error::other(format!("Failed to bring up {} via netlink: {}", iface, e))
        })?;

        Ok(())
    })
}

pub fn set_cpu_affinity(cpu: u32) -> std::io::Result<()> {
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

/// Maps kernel `CLOCK_MONOTONIC` nanosecond timestamps (as produced by
/// `ktime_get_ns()` in the rvnic driver, e.g. `RattanDesc::timestamp`) to
/// [`tokio::time::Instant`].
///
/// On Linux, `Instant` is backed by `CLOCK_MONOTONIC` as well, so the two clocks
/// differ only by a constant offset. `Instant` cannot be built from raw
/// nanoseconds, so we pair one `Instant` with its raw clock value once and
/// convert relative to that anchor afterwards, avoiding any syscall on the hot
/// path.
#[derive(Debug, Clone, Copy)]
pub struct MonotonicAnchor {
    ns: u64,
    instant: tokio::time::Instant,
}

/// Maximum disagreement tolerated between the exact (parsed) anchor and the
/// bracketed estimate before the former is considered bogus.
const ANCHOR_TOLERANCE_NS: u64 = 100_000;

fn monotonic_ns() -> u64 {
    let ts = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)
        .expect("CLOCK_MONOTONIC is always available");
    ts.tv_sec() as u64 * 1_000_000_000 + ts.tv_nsec() as u64
}

/// Recovers the raw clock value of a `std::time::Instant` from its `Debug`
/// output, which on Unix is `Instant { tv_sec: <s>, tv_nsec: <ns> }`.
///
/// The format is not a stable API, so this returns `None` on anything
/// unexpected; callers must validate the result anyway.
fn parse_instant_debug(s: &str) -> Option<u64> {
    let field = |name: &str| -> Option<u64> {
        let rest = &s[s.find(name)? + name.len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end].parse().ok()
    };
    let sec = field("tv_sec: ")?;
    let nsec = field("tv_nsec: ")?;
    (nsec < 1_000_000_000).then(|| sec.checked_mul(1_000_000_000)?.checked_add(nsec))?
}

impl MonotonicAnchor {
    pub fn new() -> Self {
        let estimated = Self::estimated();
        match Self::exact() {
            Some(exact) if exact.agrees_with(estimated) => exact,
            exact => {
                tracing::warn!(
                    target = "rvnic",
                    ?exact,
                    ?estimated,
                    "Exact monotonic anchor unavailable, using estimated one"
                );
                estimated
            }
        }
    }

    /// Exact anchor: read the raw clock value of an `Instant` directly.
    fn exact() -> Option<Self> {
        let instant = tokio::time::Instant::now();
        let ns = parse_instant_debug(&format!("{:?}", instant.into_std()))?;
        Some(Self { ns, instant })
    }

    /// Estimated anchor: bracket `Instant::now()` between two raw clock reads
    /// and keep the tightest window out of a few tries. Off by a constant of
    /// (roughly) half a clock read.
    fn estimated() -> Self {
        let mut best: Option<(u64, Self)> = None;
        for _ in 0..8 {
            let before = monotonic_ns();
            let instant = tokio::time::Instant::now();
            let after = monotonic_ns();
            let window = after - before;
            if best.as_ref().is_none_or(|(w, _)| window < *w) {
                let ns = before + window / 2;
                best = Some((window, Self { ns, instant }));
            }
        }
        best.unwrap().1
    }

    /// Whether both anchors describe the same clock, up to [`ANCHOR_TOLERANCE_NS`].
    fn agrees_with(self, other: Self) -> bool {
        // Map `other.ns` through `self`, then compare against `other.instant`.
        let mapped = self.to_instant(other.ns);
        let diff = if mapped > other.instant {
            mapped - other.instant
        } else {
            other.instant - mapped
        };
        tracing::info!(target = "rvnic", "Difference between anchors: {:?}", diff);
        diff.as_nanos() < ANCHOR_TOLERANCE_NS as u128
    }

    /// Converts a kernel `CLOCK_MONOTONIC` timestamp in nanoseconds to an `Instant`.
    #[inline(always)]
    pub fn to_instant(self, ns: u64) -> tokio::time::Instant {
        use std::time::Duration;
        if ns >= self.ns {
            self.instant + Duration::from_nanos(ns - self.ns)
        } else {
            self.instant
                .checked_sub(Duration::from_nanos(self.ns - ns))
                .unwrap_or(self.instant)
        }
    }
}

impl Default for MonotonicAnchor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{monotonic_ns, parse_instant_debug, MonotonicAnchor};
    use std::time::Duration;
    use test_log::test;

    #[test]
    fn monotonic_anchor_matches_instant() {
        let anchor = MonotonicAnchor::new();
        std::thread::sleep(Duration::from_millis(5));
        let ts = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC).unwrap();
        let ns = ts.tv_sec() as u64 * 1_000_000_000 + ts.tv_nsec() as u64;
        let now = tokio::time::Instant::now();
        let mapped = anchor.to_instant(ns);
        let diff = if now > mapped {
            now - mapped
        } else {
            mapped - now
        };
        assert!(diff < Duration::from_micros(1), "diff = {diff:?}");
        // Timestamps before the anchor map to earlier instants.
        assert!(anchor.to_instant(ns - 1_000_000) < mapped);
    }

    #[test]
    fn exact_anchor_is_available_and_precise() {
        // Holds on Linux with the current std `Debug` format; if this starts
        // failing after a toolchain bump, `MonotonicAnchor` silently falls back
        // to the estimated anchor.
        let before = monotonic_ns();
        let parsed = parse_instant_debug(&format!("{:?}", std::time::Instant::now()))
            .expect("parsable Debug");
        let after = monotonic_ns();
        tracing::info!("{} <= {} (Parsed) <= {}", before, parsed, after);
        assert!(
            before <= parsed && parsed <= after,
            "{before} <= {parsed} <= {after}"
        );
        let exact = MonotonicAnchor::exact().unwrap();
        assert!(exact.agrees_with(MonotonicAnchor::estimated()));
    }

    #[test]
    fn parse_instant_debug_rejects_garbage() {
        assert_eq!(
            parse_instant_debug("Instant { tv_sec: 3, tv_nsec: 5 }"),
            Some(3_000_000_005)
        );
        assert_eq!(parse_instant_debug("Instant(12345)"), None);
        assert_eq!(
            parse_instant_debug("Instant { tv_sec: 3, tv_nsec: 1000000000 }"),
            None
        );
    }
}
