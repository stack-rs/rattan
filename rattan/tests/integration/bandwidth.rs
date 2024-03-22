/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test bandwidth --all-features -- --nocapture
use netem_trace::{
    model::{BwTraceConfig, RepeatedBwPatternConfig, StaticBwConfig},
    Bandwidth, BwTrace,
};
use rattan::devices::bandwidth::{
    queue::{
        CoDelQueue, CoDelQueueConfig, DropHeadQueue, DropHeadQueueConfig, DropTailQueue,
        DropTailQueueConfig, InfiniteQueue,
    },
    BwDevice, BwDeviceConfig, MAX_BANDWIDTH,
};
use rattan::devices::bandwidth::{BwReplayDevice, BwReplayDeviceConfig};
use rattan::devices::external::VirtualEthernet;
use rattan::devices::{ControlInterface, Device, StdPacket};
use rattan::env::{get_std_env, StdNetEnvConfig};
use rattan::metal::io::AfPacketDriver;
use rattan::metal::netns::NetNsGuard;
use rattan::{
    core::{RattanMachine, RattanMachineConfig},
    devices::bandwidth::BwType,
};
use regex::Regex;
use std::sync::mpsc;
use std::thread::sleep;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, span, warn, Instrument, Level};

#[instrument]
#[test_log::test]
fn test_bandwidth() {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();
    let right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let (control_tx, control_rx) = oneshot::channel();

    let rattan_thread_span = span!(Level::DEBUG, "rattan_thread").or_current();
    let rattan_thread = std::thread::spawn(move || {
        let _entered = rattan_thread_span.entered();
        let original_ns = _std_env.rattan_ns.enter().unwrap();
        let _left_pair_guard = _std_env.left_pair.clone();
        let _right_pair_guard = _std_env.right_pair.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(
            async move {
                let left_bw_device =
                    BwDevice::new(MAX_BANDWIDTH, InfiniteQueue::new(), BwType::default());
                let right_bw_device =
                    BwDevice::new(MAX_BANDWIDTH, InfiniteQueue::new(), BwType::default());
                let left_control_interface = left_bw_device.control_interface();
                let right_control_interface = right_bw_device.control_interface();
                if let Err(_) = control_tx.send((left_control_interface, right_control_interface)) {
                    error!("send control interface failed");
                }
                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.left_pair.right.clone(),
                );
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.right_pair.left.clone(),
                );

                let (left_bw_rx, left_bw_tx) = machine.add_device(left_bw_device);
                info!(left_bw_rx, left_bw_tx);
                let (right_bw_rx, right_bw_tx) = machine.add_device(right_bw_device);
                info!(right_bw_rx, right_bw_tx);
                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);

                machine.link_device(left_device_rx, left_bw_tx);
                machine.link_device(left_bw_rx, right_device_tx);
                machine.link_device(right_device_rx, right_bw_tx);
                machine.link_device(right_bw_rx, left_device_tx);

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8090,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    let (left_control_interface, right_control_interface) = control_rx.blocking_recv().unwrap();

    // Before set the BwDevice, the bandwidth should be around 1Gbps
    {
        let _span = span!(Level::INFO, "iperf_no_limit").entered();
        info!("try to iperf with no bandwidth limit");
        let handle = {
            let _right_ns_guard = NetNsGuard::new(right_ns.clone()).unwrap();
            std::thread::spawn(|| {
                let mut iperf_server = std::process::Command::new("iperf3")
                    .args(["-s", "-p", "9000", "-1"])
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                iperf_server.wait().unwrap();
            })
        };
        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        sleep(Duration::from_secs(1));
        let client_handle = std::process::Command::new("iperf3")
            .args([
                "-c",
                "192.168.12.1",
                "-p",
                "9000",
                "--cport",
                "10000",
                "-t",
                "10",
                "-J",
                "-R",
                "-C",
                "reno",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = client_handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        handle.join().unwrap();
        if !stderr.is_empty() {
            warn!("{}", stderr);
        }
        let re = Regex::new(r#""bits_per_second":\s*(\d+)"#).unwrap();
        let mut bandwidth = re
            .captures_iter(&stdout)
            .flat_map(|cap| cap[1].parse::<u64>())
            .step_by(2)
            .take(10)
            .collect::<Vec<_>>();
        info!(?bandwidth);
        assert!(!bandwidth.is_empty());

        bandwidth.drain(0..4);
        let bitrate = bandwidth.iter().sum::<u64>() / bandwidth.len() as u64;
        info!("bitrate: {:?}", Bandwidth::from_bps(bitrate));
    }
    // info!("====================================================");
    // After set the BwDevice, the bandwidth should be between 90-100Mbps
    std::thread::sleep(std::time::Duration::from_secs(1));
    {
        let _span = span!(Level::INFO, "iperf_with_limit").entered();
        info!("try to iperf with bandwidth limit set to 100Mbps");
        left_control_interface
            .set_config(BwDeviceConfig::new(Bandwidth::from_mbps(100), None))
            .unwrap();
        right_control_interface
            .set_config(BwDeviceConfig::new(Bandwidth::from_mbps(100), None))
            .unwrap();
        let handle = {
            let _right_ns_guard = NetNsGuard::new(right_ns.clone()).unwrap();
            std::thread::spawn(|| {
                let mut iperf_server = std::process::Command::new("iperf3")
                    .args(["-s", "-p", "9001", "-1"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                iperf_server.wait().unwrap();
            })
        };
        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        sleep(Duration::from_secs(1));
        let client_handle = std::process::Command::new("iperf3")
            .args([
                "-c",
                "192.168.12.1",
                "-p",
                "9001",
                "--cport",
                "10001",
                "-t",
                "10",
                "-J",
                "-R",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = client_handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        if !stderr.is_empty() {
            warn!("{}", stderr);
        }
        handle.join().unwrap();

        let re = Regex::new(r#""bits_per_second":\s*(\d+)"#).unwrap();
        let mut bandwidth = re
            .captures_iter(&stdout)
            .flat_map(|cap| cap[1].parse::<u64>())
            .step_by(2)
            .take(10)
            .collect::<Vec<_>>();
        info!(?bandwidth);

        bandwidth.drain(0..4);
        let bitrate = bandwidth.iter().sum::<u64>() / bandwidth.len() as u64;
        info!("bitrate: {:?}", Bandwidth::from_bps(bitrate));
        assert!(bitrate > 90000000 && bitrate < 100000000);
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}

#[instrument]
#[test_log::test]
fn test_droptail_queue() {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();
    let right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let (control_tx, control_rx) = oneshot::channel();

    let rattan_thread_span = span!(Level::DEBUG, "rattan_thread").or_current();
    let rattan_thread = std::thread::spawn(move || {
        let _entered = rattan_thread_span.entered();
        let original_ns = _std_env.rattan_ns.enter().unwrap();
        let _left_pair_guard = _std_env.left_pair.clone();
        let _right_pair_guard = _std_env.right_pair.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(
            async move {
                let left_bw_device = BwDevice::new(
                    MAX_BANDWIDTH,
                    DropTailQueue::new(10, None, BwType::default()),
                    BwType::default(),
                );
                let right_bw_device = BwDevice::new(
                    MAX_BANDWIDTH,
                    DropTailQueue::new(10, None, BwType::default()),
                    BwType::default(),
                );
                let left_control_interface = left_bw_device.control_interface();
                let right_control_interface = right_bw_device.control_interface();
                if let Err(_) = control_tx.send((left_control_interface, right_control_interface)) {
                    error!("send control interface failed");
                }
                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.left_pair.right.clone(),
                );
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.right_pair.left.clone(),
                );

                let (left_bw_rx, left_bw_tx) = machine.add_device(left_bw_device);
                info!(left_bw_rx, left_bw_tx);
                let (right_bw_rx, right_bw_tx) = machine.add_device(right_bw_device);
                info!(right_bw_rx, right_bw_tx);
                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);

                machine.link_device(left_device_rx, left_bw_tx);
                machine.link_device(left_bw_rx, right_device_tx);
                machine.link_device(right_device_rx, right_bw_tx);
                machine.link_device(right_bw_rx, left_device_tx);

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8091,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    let (left_control_interface, _right_control_interface) = control_rx.blocking_recv().unwrap();

    {
        let _span = span!(Level::INFO, "run_test").entered();
        info!("Test DropTailQueue");

        let (msg_tx, msg_rx) = mpsc::channel();

        let cancel_token_inner = CancellationToken::new();
        let server_cancel_token = cancel_token_inner.clone();
        {
            let _right_ns_guard = NetNsGuard::new(right_ns.clone()).unwrap();
            std::thread::spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let server_socket = tokio::net::UdpSocket::bind("0.0.0.0:54321").await.unwrap();
                    let mut buf = [0; 1024];
                    loop {
                        tokio::select! {
                            _ = cancel_token_inner.cancelled() => {
                                break;
                            }
                            Ok((size, _)) = server_socket.recv_from(&mut buf) => {
                                msg_tx.send((buf[0], size, Instant::now())).unwrap();
                            }
                        }
                    }
                });
            })
        };

        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        sleep(Duration::from_millis(500));

        let client_socket = std::net::UdpSocket::bind("0.0.0.0:54321").unwrap();
        client_socket.connect("192.168.12.1:54321").unwrap();

        info!("Test DropTailQueue (10 packets limit)");
        info!("Set bandwidth to 800kbps (1000B per 10ms)");
        left_control_interface
            .set_config(BwDeviceConfig::new(Bandwidth::from_kbps(800), None))
            .unwrap();
        info!("Send 30 packets(1000B) with 1.05ms interval");
        let mut next_time = Instant::now();
        for i in 0..30 {
            client_socket.send(&[i as u8; 1000 - 28]).unwrap(); // 28 = 20(IPv4) + 8(UDP)
            next_time += Duration::from_micros(1050);
            sleep(next_time - Instant::now());
        }
        sleep(Duration::from_millis(200));
        let mut recv_indexs = Vec::new();
        while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
            recv_indexs.push(index);
        }
        info!(?recv_indexs);
        assert!(recv_indexs.len() == 14);
        for i in 0..12 {
            assert!(recv_indexs[i] == i as u8);
        }
        assert!(19 <= recv_indexs[12] && recv_indexs[12] <= 20);
        assert!(27 <= recv_indexs[13] && recv_indexs[13] <= 30);

        info!("Test DropTailQueue (500 Bytes limit)");
        info!("Set bandwidth to 40kbps (50B per 10ms)");
        left_control_interface
            .set_config(BwDeviceConfig::new(
                Bandwidth::from_kbps(40),
                DropTailQueueConfig::new(None, 500, BwType::default()),
            ))
            .unwrap();
        info!("Send 30 packets(50B) with 1.05ms interval");
        let mut next_time = Instant::now();
        for i in 0..30 {
            client_socket.send(&[i as u8; 50 - 28]).unwrap(); // 28 = 20(IPv4) + 8(UDP)
            next_time += Duration::from_micros(1050);
            sleep(next_time - Instant::now());
        }
        sleep(Duration::from_millis(200));
        let mut recv_indexs = Vec::new();
        while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
            recv_indexs.push(index);
        }
        info!(?recv_indexs);
        assert!(recv_indexs.len() == 14);
        for i in 0..12 {
            assert!(recv_indexs[i] == i as u8);
        }
        assert!(19 <= recv_indexs[12] && recv_indexs[12] <= 20);
        assert!(27 <= recv_indexs[13] && recv_indexs[13] <= 30);

        server_cancel_token.cancel();
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}

#[instrument]
#[test_log::test]
fn test_drophead_queue() {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();
    let right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let (control_tx, control_rx) = oneshot::channel();

    let rattan_thread_span = span!(Level::DEBUG, "rattan_thread").or_current();
    let rattan_thread = std::thread::spawn(move || {
        let _entered = rattan_thread_span.entered();
        let original_ns = _std_env.rattan_ns.enter().unwrap();
        let _left_pair_guard = _std_env.left_pair.clone();
        let _right_pair_guard = _std_env.right_pair.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(
            async move {
                let left_bw_device = BwDevice::new(
                    MAX_BANDWIDTH,
                    DropHeadQueue::new(10, None, BwType::default()),
                    BwType::default(),
                );
                let right_bw_device = BwDevice::new(
                    MAX_BANDWIDTH,
                    DropHeadQueue::new(10, None, BwType::default()),
                    BwType::default(),
                );
                let left_control_interface = left_bw_device.control_interface();
                let right_control_interface = right_bw_device.control_interface();
                if let Err(_) = control_tx.send((left_control_interface, right_control_interface)) {
                    error!("send control interface failed");
                }
                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.left_pair.right.clone(),
                );
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.right_pair.left.clone(),
                );

                let (left_bw_rx, left_bw_tx) = machine.add_device(left_bw_device);
                info!(left_bw_rx, left_bw_tx);
                let (right_bw_rx, right_bw_tx) = machine.add_device(right_bw_device);
                info!(right_bw_rx, right_bw_tx);
                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);

                machine.link_device(left_device_rx, left_bw_tx);
                machine.link_device(left_bw_rx, right_device_tx);
                machine.link_device(right_device_rx, right_bw_tx);
                machine.link_device(right_bw_rx, left_device_tx);

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8092,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    let (left_control_interface, _right_control_interface) = control_rx.blocking_recv().unwrap();

    {
        let _span = span!(Level::INFO, "run_test").entered();
        info!("Test DropHeadQueue");

        let (msg_tx, msg_rx) = mpsc::channel();

        let cancel_token_inner = CancellationToken::new();
        let server_cancel_token = cancel_token_inner.clone();
        {
            let _right_ns_guard = NetNsGuard::new(right_ns.clone()).unwrap();
            std::thread::spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let server_socket = tokio::net::UdpSocket::bind("0.0.0.0:54321").await.unwrap();
                    let mut buf = [0; 1024];
                    loop {
                        tokio::select! {
                            _ = cancel_token_inner.cancelled() => {
                                break;
                            }
                            Ok((size, _)) = server_socket.recv_from(&mut buf) => {
                                msg_tx.send((buf[0], size, Instant::now())).unwrap();
                            }
                        }
                    }
                });
            })
        };

        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        sleep(Duration::from_millis(500));

        let client_socket = std::net::UdpSocket::bind("0.0.0.0:54321").unwrap();
        client_socket.connect("192.168.12.1:54321").unwrap();

        info!("Test DropHeadQueue (10 packets limit)");
        info!("Set bandwidth to 800kbps (1000B per 10ms)");
        left_control_interface
            .set_config(BwDeviceConfig::new(Bandwidth::from_kbps(800), None))
            .unwrap();
        info!("Send 30 packets(1000B) with 1.05ms interval");
        let mut next_time = Instant::now();
        for i in 0..30 {
            client_socket.send(&[i as u8; 1000 - 28]).unwrap(); // 28 = 20(IPv4) + 8(UDP)
            next_time += Duration::from_micros(1050);
            sleep(next_time - Instant::now());
        }
        sleep(Duration::from_millis(200));
        let mut recv_indexs = Vec::new();
        while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
            recv_indexs.push(index);
        }
        info!(?recv_indexs);
        assert!(recv_indexs.len() == 14);
        assert!(recv_indexs[0] == 0);
        assert!(recv_indexs[1] == 1);
        assert!(8 <= recv_indexs[2] && recv_indexs[2] <= 10);
        assert!(17 <= recv_indexs[3] && recv_indexs[3] <= 19);
        for i in 4..14 {
            assert!(recv_indexs[i] == 16 + i as u8);
        }

        info!("Test DropHeadQueue (500 Bytes limit)");
        info!("Set bandwidth to 40kbps (50B per 10ms)");
        left_control_interface
            .set_config(BwDeviceConfig::new(
                Bandwidth::from_kbps(40),
                DropHeadQueueConfig::new(None, 500, BwType::default()),
            ))
            .unwrap();
        info!("Send 30 packets(50B) with 1.05ms interval");
        let mut next_time = Instant::now();
        for i in 0..30 {
            client_socket.send(&[i as u8; 50 - 28]).unwrap(); // 28 = 20(IPv4) + 8(UDP)
            next_time += Duration::from_micros(1050);
            sleep(next_time - Instant::now());
        }
        sleep(Duration::from_millis(200));
        let mut recv_indexs = Vec::new();
        while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
            recv_indexs.push(index);
        }
        info!(?recv_indexs);
        assert!(recv_indexs.len() == 14);
        assert!(recv_indexs[0] == 0);
        assert!(recv_indexs[1] == 1);
        assert!(8 <= recv_indexs[2] && recv_indexs[2] <= 10);
        assert!(17 <= recv_indexs[3] && recv_indexs[3] <= 19);
        for i in 4..14 {
            assert!(recv_indexs[i] == 16 + i as u8);
        }

        server_cancel_token.cancel();
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}

#[instrument]
#[test_log::test]
fn test_codel_queue() {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();
    let right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let (control_tx, control_rx) = oneshot::channel();

    let rattan_thread_span = span!(Level::DEBUG, "rattan_thread").or_current();
    let rattan_thread = std::thread::spawn(move || {
        let _entered = rattan_thread_span.entered();
        let original_ns = _std_env.rattan_ns.enter().unwrap();
        let _left_pair_guard = _std_env.left_pair.clone();
        let _right_pair_guard = _std_env.right_pair.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(
            async move {
                let left_bw_device = BwDevice::new(
                    MAX_BANDWIDTH,
                    CoDelQueue::new(CoDelQueueConfig::new(
                        60,
                        None,
                        Duration::from_millis(104),
                        Duration::from_millis(50),
                        1500,
                        BwType::default(),
                    )),
                    BwType::default(),
                );
                let right_bw_device =
                    BwDevice::new(MAX_BANDWIDTH, InfiniteQueue::new(), BwType::default());
                let left_control_interface = left_bw_device.control_interface();
                let right_control_interface = right_bw_device.control_interface();
                if let Err(_) = control_tx.send((left_control_interface, right_control_interface)) {
                    error!("send control interface failed");
                }
                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.left_pair.right.clone(),
                );
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.right_pair.left.clone(),
                );

                let (left_bw_rx, left_bw_tx) = machine.add_device(left_bw_device);
                info!(left_bw_rx, left_bw_tx);
                let (right_bw_rx, right_bw_tx) = machine.add_device(right_bw_device);
                info!(right_bw_rx, right_bw_tx);
                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);

                machine.link_device(left_device_rx, left_bw_tx);
                machine.link_device(left_bw_rx, right_device_tx);
                machine.link_device(right_device_rx, right_bw_tx);
                machine.link_device(right_bw_rx, left_device_tx);

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8093,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    let (left_control_interface, _right_control_interface) = control_rx.blocking_recv().unwrap();

    {
        let _span = span!(Level::INFO, "run_test").entered();
        info!("Test CoDelQueue");

        let (msg_tx, msg_rx) = mpsc::channel();

        let cancel_token_inner = CancellationToken::new();
        let server_cancel_token = cancel_token_inner.clone();
        {
            let _right_ns_guard = NetNsGuard::new(right_ns.clone()).unwrap();
            std::thread::spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let server_socket = tokio::net::UdpSocket::bind("0.0.0.0:54321").await.unwrap();
                    let mut buf = [0; 1024];
                    loop {
                        tokio::select! {
                            _ = cancel_token_inner.cancelled() => {
                                break;
                            }
                            Ok((size, _)) = server_socket.recv_from(&mut buf) => {
                                msg_tx.send((buf[0], size, Instant::now())).unwrap();
                            }
                        }
                    }
                });
            })
        };

        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        sleep(Duration::from_millis(500));

        let client_socket = std::net::UdpSocket::bind("0.0.0.0:54321").unwrap();
        client_socket.connect("192.168.12.1:54321").unwrap();

        info!("Test CoDelQueue (60 packets limit, target 50ms, interval 104ms, mtu 1500)");
        info!("Set bandwidth to 800kbps (1000B per 10ms)");
        left_control_interface
            .set_config(BwDeviceConfig::new(Bandwidth::from_kbps(800), None))
            .unwrap();
        info!("Send 80 packets(1000B) with 2.05ms interval");
        let mut next_time = Instant::now();
        for i in 0..80 {
            client_socket.send(&[i as u8; 1000 - 28]).unwrap(); // 28 = 20(IPv4) + 8(UDP)
            next_time += Duration::from_micros(2050);
            sleep(next_time - Instant::now());
        }
        sleep(Duration::from_millis(1000));
        let mut recv_indexs = Vec::new();
        while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
            recv_indexs.push(index);
        }
        info!(?recv_indexs);
        let mut dropped_indexs = Vec::new();
        for i in 0..80 {
            if !recv_indexs.contains(&(i as u8)) {
                dropped_indexs.push(i as u8);
            }
        }
        info!(?dropped_indexs);
        // A reference result: [18, 30, 38, 45, 51, 57, 62, 67, 72, 76, 77, 78] (len=12)
        // This result is same with mahimahi
        assert!(11 <= dropped_indexs.len() && dropped_indexs.len() <= 13);
        assert!(17 <= dropped_indexs[0] && dropped_indexs[0] <= 19);
        let pattarn = [12, 8, 7, 6, 6, 5, 5, 4];
        for i in 0..pattarn.len() {
            assert!(
                pattarn[i] - 1 <= dropped_indexs[i + 1] - dropped_indexs[i]
                    && dropped_indexs[i + 1] - dropped_indexs[i] <= pattarn[i] + 1
            );
        }

        info!("Test CoDelQueue (500 Bytes limit, target 50ms, interval 104ms, mtu 80)");
        info!("Use last cycle as a good starting point");
        info!("Set bandwidth to 40kbps (50B per 10ms)");
        left_control_interface
            .set_config(BwDeviceConfig::new(
                Bandwidth::from_kbps(40),
                CoDelQueueConfig::new(
                    None,
                    3000,
                    Duration::from_millis(104),
                    Duration::from_millis(50),
                    80,
                    BwType::default(),
                ),
            ))
            .unwrap();
        info!("Send 80 packets(50B) with 2.05ms interval");
        let mut next_time = Instant::now();
        for i in 0..80 {
            client_socket.send(&[i as u8; 50 - 28]).unwrap(); // 28 = 20(IPv4) + 8(UDP)
            next_time += Duration::from_micros(2050);
            sleep(next_time - Instant::now());
        }
        sleep(Duration::from_millis(1000));
        let mut recv_indexs = Vec::new();
        while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
            recv_indexs.push(index);
        }
        info!(?recv_indexs);
        let mut dropped_indexs = Vec::new();
        for i in 0..80 {
            if !recv_indexs.contains(&(i as u8)) {
                dropped_indexs.push(i as u8);
            }
        }
        info!(?dropped_indexs);
        // A reference result: [18, 23, 28, 32, 36, 40, 44, 48, 51, 55, 59, 62, 65, 69, 72, 76, 77, 78] (len=18)
        assert!(16 <= dropped_indexs.len() && dropped_indexs.len() <= 20);
        assert!(16 <= dropped_indexs[0] && dropped_indexs[0] <= 20);
        let pattarn = [5, 5, 4, 4, 4, 4, 3, 4, 4, 3, 3, 4, 3];
        for i in 0..pattarn.len() {
            assert!(
                pattarn[i] - 1 <= dropped_indexs[i + 1] - dropped_indexs[i]
                    && dropped_indexs[i + 1] - dropped_indexs[i] <= pattarn[i] + 1
            );
        }

        server_cancel_token.cancel();
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}

#[instrument]
#[test_log::test]
fn test_replay() {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();
    let right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let (control_tx, control_rx) = oneshot::channel();

    let rattan_thread_span = span!(Level::DEBUG, "rattan_thread").or_current();
    let rattan_thread = std::thread::spawn(move || {
        let _entered = rattan_thread_span.entered();
        let original_ns = _std_env.rattan_ns.enter().unwrap();
        let _left_pair_guard = _std_env.left_pair.clone();
        let _right_pair_guard = _std_env.right_pair.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(
            async move {
                let trace = RepeatedBwPatternConfig::new()
                    .pattern(vec![Box::new(StaticBwConfig {
                        bw: Some(Bandwidth::from_mbps(1)),
                        duration: Some(Duration::from_secs(10)),
                    }) as Box<dyn BwTraceConfig>])
                    .build();
                let left_bw_device = BwReplayDevice::new(
                    Box::new(trace) as Box<dyn BwTrace>,
                    DropHeadQueue::new(100, None, BwType::default()),
                    BwType::default(),
                );
                let right_bw_device =
                    BwDevice::new(MAX_BANDWIDTH, InfiniteQueue::new(), BwType::default());
                let left_control_interface = left_bw_device.control_interface();
                let right_control_interface = right_bw_device.control_interface();
                if let Err(_) = control_tx.send((left_control_interface, right_control_interface)) {
                    error!("send control interface failed");
                }
                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.left_pair.right.clone(),
                );
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.right_pair.left.clone(),
                );

                let (left_bw_rx, left_bw_tx) = machine.add_device(left_bw_device);
                info!(left_bw_rx, left_bw_tx);
                let (right_bw_rx, right_bw_tx) = machine.add_device(right_bw_device);
                info!(right_bw_rx, right_bw_tx);
                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);

                machine.link_device(left_device_rx, left_bw_tx);
                machine.link_device(left_bw_rx, right_device_tx);
                machine.link_device(right_device_rx, right_bw_tx);
                machine.link_device(right_bw_rx, left_device_tx);

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8094,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    let (left_control_interface, _right_control_interface) = control_rx.blocking_recv().unwrap();

    std::thread::sleep(std::time::Duration::from_secs(1));
    {
        let _span = span!(Level::INFO, "test_replay").entered();
        let handle = {
            let _right_ns_guard = NetNsGuard::new(right_ns.clone()).unwrap();
            std::thread::spawn(|| {
                let mut iperf_server = std::process::Command::new("iperf3")
                    .args(["-s", "-p", "9001", "-1"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                iperf_server.wait().unwrap();
            })
        };
        sleep(Duration::from_secs(1));
        info!("try to iperf with bw limit 5s 100Mbps and 5s 20Mbps");
        let trace_config = RepeatedBwPatternConfig::new().pattern(vec![
            Box::new(StaticBwConfig {
                bw: Some(Bandwidth::from_mbps(100)),
                duration: Some(Duration::from_secs(5)),
            }),
            Box::new(StaticBwConfig {
                bw: Some(Bandwidth::from_mbps(20)),
                duration: Some(Duration::from_secs(5)),
            }) as Box<dyn BwTraceConfig>,
        ]);
        left_control_interface
            .set_config(BwReplayDeviceConfig::new(
                Box::new(trace_config) as Box<dyn BwTraceConfig>,
                None,
            ))
            .unwrap();
        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        let client_handle = std::process::Command::new("iperf3")
            .args([
                "-c",
                "192.168.12.1",
                "-p",
                "9001",
                "--cport",
                "10001",
                "-t",
                "10",
                "-J",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = client_handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        if !stderr.is_empty() {
            warn!("{}", stderr);
        }
        handle.join().unwrap();

        let re = Regex::new(r#""bits_per_second":\s*(\d+)"#).unwrap();
        let mut bandwidth = re
            .captures_iter(&stdout)
            .flat_map(|cap| cap[1].parse::<u64>())
            .step_by(2)
            .take(10)
            .collect::<Vec<_>>();
        let front_5s_bandwidth = bandwidth.drain(0..5).collect::<Vec<_>>();
        let back_5s_bandwidth = bandwidth.drain(0..5).collect::<Vec<_>>();

        let front_5s_bitrate =
            front_5s_bandwidth.iter().sum::<u64>() / front_5s_bandwidth.len() as u64;
        let back_5s_bitrate =
            back_5s_bandwidth.iter().sum::<u64>() / back_5s_bandwidth.len() as u64;

        info!(
            "Front 5s bitrate: {:?}, bandwidth: {:?}",
            Bandwidth::from_bps(front_5s_bitrate),
            front_5s_bandwidth
        );
        info!(
            "Back 5s bitrate: {:?}, bandwidth: {:?}",
            Bandwidth::from_bps(back_5s_bitrate),
            back_5s_bandwidth
        );

        assert!(front_5s_bitrate > 90000000 && front_5s_bitrate < 100000000);
        assert!(back_5s_bitrate > 18000000 && back_5s_bitrate < 21000000);
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}
