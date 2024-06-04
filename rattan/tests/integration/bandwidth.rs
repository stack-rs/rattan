/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test bandwidth --all-features -- --nocapture
use std::thread::sleep;
use std::time::{Duration, Instant};
use std::{collections::HashMap, sync::mpsc};

use netem_trace::{
    model::{BwTraceConfig, RepeatedBwPatternConfig, StaticBwConfig},
    Bandwidth, BwTrace,
};
use rattan::devices::{
    bandwidth::{
        queue::{
            CoDelQueue, CoDelQueueConfig, DropHeadQueue, DropHeadQueueConfig, DropTailQueue,
            DropTailQueueConfig, InfiniteQueue, InfiniteQueueConfig,
        },
        BwDeviceConfig, BwReplayDevice, BwReplayDeviceConfig, BwType,
    },
    ControlInterface, StdPacket,
};
use rattan::env::{StdNetEnvConfig, StdNetEnvMode};
use rattan::radix::RattanRadix;
use rattan::{
    config::{BwDeviceBuildConfig, DeviceBuildConfig, RattanConfig},
    control::RattanOp,
};
use regex::Regex;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, span, warn, Level};

#[instrument]
#[test_log::test]
fn test_bandwidth() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
        },
        ..Default::default()
    };
    config.core.devices.insert(
        "up_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::Infinite(BwDeviceConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.core.devices.insert(
        "down_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::Infinite(BwDeviceConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.core.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<StdPacket>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Before config the BwDevice, the bandwidth should be around 1Gbps
    {
        let _span = span!(Level::INFO, "iperf_no_limit").entered();
        info!("try to iperf with no bandwidth limit");
        let right_handle = radix
            .right_spawn(|| {
                let mut iperf_server = std::process::Command::new("iperf3")
                    .args(["-s", "-p", "9000", "-1"])
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                iperf_server.wait().unwrap();
                Ok(())
            })
            .unwrap();
        sleep(Duration::from_millis(500));
        let left_handle = radix
            .left_spawn(|| {
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
                Ok(client_handle.wait_with_output())
            })
            .unwrap();
        let output = left_handle.join().unwrap().unwrap().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        right_handle.join().unwrap().unwrap();
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

    // After set the BwDevice, the bandwidth should be between 90-100Mbps
    std::thread::sleep(std::time::Duration::from_millis(100));
    {
        let _span = span!(Level::INFO, "iperf_with_limit").entered();
        info!("try to iperf with bandwidth limit set to 100Mbps");
        radix
            .op_block_exec(RattanOp::ConfigDevice(
                "down_bw".to_string(),
                serde_json::to_value(BwDeviceConfig::<StdPacket, InfiniteQueue<StdPacket>>::new(
                    Bandwidth::from_mbps(100),
                    None,
                    None,
                ))
                .unwrap(),
            ))
            .unwrap();

        let right_handle = radix
            .right_spawn(|| {
                let mut iperf_server = std::process::Command::new("iperf3")
                    .args(["-s", "-p", "9001", "-1"])
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                iperf_server.wait().unwrap();
                Ok(())
            })
            .unwrap();
        sleep(Duration::from_millis(500));
        let left_handle = radix
            .left_spawn(|| {
                let client_handle = std::process::Command::new("iperf3")
                    .args([
                        "-c",
                        "192.168.12.1",
                        "-p",
                        "9001",
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
                Ok(client_handle.wait_with_output())
            })
            .unwrap();
        let output = left_handle.join().unwrap().unwrap().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        if !stderr.is_empty() {
            warn!("{}", stderr);
        }
        right_handle.join().unwrap().unwrap();

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
}

#[instrument]
#[test_log::test]
fn test_droptail_queue() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
        },
        ..Default::default()
    };
    config.core.devices.insert(
        "up_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::DropTail(BwDeviceConfig::new(
            None,
            DropTailQueueConfig::new(None, None, BwType::default()),
            None,
        ))),
    );
    config.core.devices.insert(
        "down_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::DropTail(BwDeviceConfig::new(
            None,
            DropTailQueueConfig::new(None, None, BwType::default()),
            None,
        ))),
    );
    config.core.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<StdPacket>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    {
        let _span = span!(Level::INFO, "run_test").entered();
        info!("Test DropTailQueue");

        let (msg_tx, msg_rx) = mpsc::channel();

        let cancel_token_inner = CancellationToken::new();
        let server_cancel_token = cancel_token_inner.clone();

        let right_handle = radix
            .right_spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
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
                Ok(())
            })
            .unwrap();

        let op_endpoint = radix.op_endpoint();
        let left_handle = radix
            .left_spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                sleep(Duration::from_millis(100));
                let client_socket = std::net::UdpSocket::bind("0.0.0.0:54321").unwrap();
                client_socket.connect("192.168.12.1:54321").unwrap();

                info!("Test DropTailQueue (10 packets limit)");
                info!("Set bandwidth to 800kbps (1000B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigDevice(
                            "up_bw".to_string(),
                            serde_json::to_value(BwDeviceConfig::<
                                StdPacket,
                                DropTailQueue<StdPacket>,
                            >::new(
                                Bandwidth::from_kbps(800),
                                DropTailQueueConfig::new(10, None, BwType::default()),
                                None,
                            ))
                            .unwrap(),
                        )),
                    )
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
                for (i, recv_index) in recv_indexs.iter().enumerate().take(12) {
                    assert!(*recv_index == i as u8);
                }
                assert!(19 <= recv_indexs[12] && recv_indexs[12] <= 20);
                assert!(27 <= recv_indexs[13] && recv_indexs[13] <= 30);

                info!("Test DropTailQueue (500 Bytes limit)");
                info!("Set bandwidth to 40kbps (50B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigDevice(
                            "up_bw".to_string(),
                            serde_json::to_value(BwDeviceConfig::<
                                StdPacket,
                                DropTailQueue<StdPacket>,
                            >::new(
                                Bandwidth::from_kbps(40),
                                DropTailQueueConfig::new(None, 500, BwType::default()),
                                None,
                            ))
                            .unwrap(),
                        )),
                    )
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
                for (i, recv_index) in recv_indexs.iter().enumerate().take(12) {
                    assert!(*recv_index == i as u8);
                }
                assert!(19 <= recv_indexs[12] && recv_indexs[12] <= 20);
                assert!(27 <= recv_indexs[13] && recv_indexs[13] <= 30);
                Ok(())
            })
            .unwrap();

        left_handle.join().unwrap().unwrap();
        server_cancel_token.cancel();
        right_handle.join().unwrap().unwrap();
    }
}

#[instrument]
#[test_log::test]
fn test_drophead_queue() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
        },
        ..Default::default()
    };
    config.core.devices.insert(
        "up_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::DropHead(BwDeviceConfig::new(
            None,
            DropHeadQueueConfig::new(None, None, BwType::default()),
            None,
        ))),
    );
    config.core.devices.insert(
        "down_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::DropHead(BwDeviceConfig::new(
            None,
            DropHeadQueueConfig::new(None, None, BwType::default()),
            None,
        ))),
    );
    config.core.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<StdPacket>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    {
        let _span = span!(Level::INFO, "run_test").entered();
        info!("Test DropHeadQueue");

        let (msg_tx, msg_rx) = mpsc::channel();

        let cancel_token_inner = CancellationToken::new();
        let server_cancel_token = cancel_token_inner.clone();

        let right_handle = radix
            .right_spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
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
                Ok(())
            })
            .unwrap();

        let op_endpoint = radix.op_endpoint();
        let left_handle = radix
            .left_spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                sleep(Duration::from_millis(100));
                let client_socket = std::net::UdpSocket::bind("0.0.0.0:54321").unwrap();
                client_socket.connect("192.168.12.1:54321").unwrap();

                info!("Test DropHeadQueue (10 packets limit)");
                info!("Set bandwidth to 800kbps (1000B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigDevice(
                            "up_bw".to_string(),
                            serde_json::to_value(BwDeviceConfig::<
                                StdPacket,
                                DropHeadQueue<StdPacket>,
                            >::new(
                                Bandwidth::from_kbps(800),
                                DropHeadQueueConfig::new(10, None, BwType::default()),
                                None,
                            ))
                            .unwrap(),
                        )),
                    )
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
                for (i, recv_index) in recv_indexs.iter().enumerate().take(14).skip(4) {
                    assert!(*recv_index == 16 + i as u8);
                }

                info!("Test DropHeadQueue (500 Bytes limit)");
                info!("Set bandwidth to 40kbps (50B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigDevice(
                            "up_bw".to_string(),
                            serde_json::to_value(BwDeviceConfig::<
                                StdPacket,
                                DropHeadQueue<StdPacket>,
                            >::new(
                                Bandwidth::from_kbps(40),
                                DropHeadQueueConfig::new(None, 500, BwType::default()),
                                None,
                            ))
                            .unwrap(),
                        )),
                    )
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
                for (i, recv_index) in recv_indexs.iter().enumerate().take(14).skip(4) {
                    assert!(*recv_index == 16 + i as u8);
                }
                Ok(())
            })
            .unwrap();

        left_handle.join().unwrap().unwrap();
        server_cancel_token.cancel();
        right_handle.join().unwrap().unwrap();
    }
}

#[instrument]
#[test_log::test]
fn test_codel_queue() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
        },
        ..Default::default()
    };
    config.core.devices.insert(
        "up_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::CoDel(BwDeviceConfig::new(
            None,
            CoDelQueueConfig::new(
                60,
                None,
                Duration::from_millis(104),
                Duration::from_millis(50),
                1500,
                BwType::default(),
            ),
            None,
        ))),
    );
    config.core.devices.insert(
        "down_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::Infinite(BwDeviceConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.core.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<StdPacket>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    {
        let _span = span!(Level::INFO, "run_test").entered();
        info!("Test CoDelQueue");

        let (msg_tx, msg_rx) = mpsc::channel();

        let cancel_token_inner = CancellationToken::new();
        let server_cancel_token = cancel_token_inner.clone();

        let right_handle = radix
            .right_spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
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
                Ok(())
            })
            .unwrap();

        let op_endpoint = radix.op_endpoint();
        let left_handle = radix
            .left_spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                sleep(Duration::from_millis(100));
                let client_socket = std::net::UdpSocket::bind("0.0.0.0:54321").unwrap();
                client_socket.connect("192.168.12.1:54321").unwrap();

                info!("Test CoDelQueue (60 packets limit, target 50ms, interval 104ms, mtu 1500)");
                info!("Set bandwidth to 800kbps (1000B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigDevice(
                            "up_bw".to_string(),
                            serde_json::to_value(
                                BwDeviceConfig::<StdPacket, CoDelQueue<StdPacket>>::new(
                                    Bandwidth::from_kbps(800),
                                    None,
                                    None,
                                ),
                            )
                            .unwrap(),
                        )),
                    )
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
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigDevice(
                            "up_bw".to_string(),
                            serde_json::to_value(
                                BwDeviceConfig::<StdPacket, CoDelQueue<StdPacket>>::new(
                                    Bandwidth::from_kbps(40),
                                    CoDelQueueConfig::new(
                                        None,
                                        3000,
                                        Duration::from_millis(104),
                                        Duration::from_millis(50),
                                        80,
                                        BwType::default(),
                                    ),
                                    None,
                                ),
                            )
                            .unwrap(),
                        )),
                    )
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
                Ok(())
            })
            .unwrap();

        left_handle.join().unwrap().unwrap();
        server_cancel_token.cancel();
        right_handle.join().unwrap().unwrap();
    }
}

#[instrument]
#[test_log::test]
fn test_replay() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
        },
        ..Default::default()
    };
    config
        .core
        .devices
        .insert("up_bw".to_string(), DeviceBuildConfig::Custom);
    config.core.devices.insert(
        "down_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::Infinite(BwDeviceConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.core.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<StdPacket>::new(config).unwrap();
    let control_interface = radix
        .build_deivce("up_bw".to_string(), |handle| {
            let _guard = handle.enter();
            let trace = RepeatedBwPatternConfig::new()
                .pattern(vec![Box::new(StaticBwConfig {
                    bw: Some(Bandwidth::from_mbps(1)),
                    duration: Some(Duration::from_secs(10)),
                }) as Box<dyn BwTraceConfig>])
                .build();
            BwReplayDevice::new(
                Box::new(trace) as Box<dyn BwTrace>,
                DropTailQueue::new(DropTailQueueConfig::new(100, None, BwType::default())),
                None,
            )
        })
        .unwrap();
    radix.spawn_rattan().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(100));
    {
        let _span = span!(Level::INFO, "test_replay").entered();
        let right_handle = radix
            .right_spawn(|| {
                let mut iperf_server = std::process::Command::new("iperf3")
                    .args(["-s", "-p", "9000", "-1"])
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                iperf_server.wait().unwrap();
                Ok(())
            })
            .unwrap();
        sleep(Duration::from_millis(500));
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
        control_interface
            .set_config(BwReplayDeviceConfig::new(
                Box::new(trace_config) as Box<dyn BwTraceConfig>,
                None,
                None,
            ))
            .unwrap();
        let left_handle = radix
            .left_spawn(|| {
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
                    ])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                Ok(client_handle.wait_with_output())
            })
            .unwrap();

        radix.start_rattan().unwrap();
        let output = left_handle.join().unwrap().unwrap().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        if !stderr.is_empty() {
            warn!("{}", stderr);
        }
        right_handle.join().unwrap().unwrap();

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
        assert!(back_5s_bitrate > 18000000 && back_5s_bitrate < 22000000);
    }
}
