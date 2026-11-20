/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test bandwidth --all-features -- --nocapture
use std::{collections::HashMap, thread::sleep, time::Duration};
#[cfg(feature = "serde")]
use std::{sync::mpsc, time::Instant};

use itertools::Itertools;
use netem_trace::{
    model::{BwTraceConfig, RepeatedBwPatternConfig, StaticBwConfig},
    Bandwidth, BwTrace,
};
#[cfg(feature = "serde")]
use rattan_core::{
    cells::bandwidth::queue::{
        CoDelQueue, CoDelQueueConfig, DropHeadQueue, DropHeadQueueConfig, InfiniteQueue,
    },
    control::RattanOp,
};
use rattan_core::{
    cells::{
        bandwidth::{
            queue::{
                DropTailQueue, DropTailQueueConfig, DualPi2QueueConfig, InfiniteQueueConfig,
                PacketQueue,
            },
            BwCellConfig, BwReplayCell, BwReplayCellConfig, BwType,
        },
        ControlInterface,
    },
    config::{BwCellBuildConfig, CellBuildConfig, RattanConfig},
    radix::RattanRadix,
};
use rattan_env::{
    env::standard::{AfPacketDriver, StdNetEnv, StdNetEnvConfig, StdPacket},
    RattanEnv, StdNetEnvMode,
};

use regex::Regex;
#[cfg(feature = "serde")]
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, span, warn, Level};

#[instrument]
#[test_log::test]
#[serial_test::parallel]
fn test_bandwidth() {
    let mut config = RattanConfig::<StdPacket, StdNetEnvConfig> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
            ..Default::default()
        },
        ..Default::default()
    };
    config.cells.insert(
        "up_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::Infinite(BwCellConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.cells.insert(
        "down_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::Infinite(BwCellConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver, StdNetEnv>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Before config the BwCell, the bandwidth should be around 1Gbps
    {
        let _span = span!(Level::INFO, "iperf_no_limit").entered();
        info!("try to iperf with no bandwidth limit");
        let right_handle = radix
            .right_spawn(None, || {
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
        let right_ip = <StdNetEnv as RattanEnv<AfPacketDriver>>::right_ip(&radix, 1).to_string();
        let left_handle = radix
            .left_spawn(None, move || {
                let client_handle = std::process::Command::new("iperf3")
                    .args([
                        "-c", &right_ip, "-p", "9000", "--cport", "10000", "-t", "10", "-J", "-R",
                        "-C", "reno",
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

    // After set the BwCell, the bandwidth should be between 90-100Mbps
    std::thread::sleep(std::time::Duration::from_millis(100));
    #[cfg(feature = "serde")]
    {
        let _span = span!(Level::INFO, "iperf_with_limit").entered();
        info!("try to iperf with bandwidth limit set to 100Mbps");
        radix
            .op_block_exec(RattanOp::ConfigCell(
                "down_bw".to_string(),
                serde_json::to_value(BwCellConfig::<StdPacket, InfiniteQueue<StdPacket>>::new(
                    Bandwidth::from_mbps(100),
                    None,
                    None,
                ))
                .unwrap(),
            ))
            .unwrap();

        let right_handle = radix
            .right_spawn(None, || {
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
        let right_ip = <StdNetEnv as RattanEnv<AfPacketDriver>>::right_ip(&radix, 1).to_string();
        let left_handle = radix
            .left_spawn(None, move || {
                let client_handle = std::process::Command::new("iperf3")
                    .args([
                        "-c", &right_ip, "-p", "9001", "--cport", "10000", "-t", "10", "-J", "-R",
                        "-C", "reno",
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

#[cfg(feature = "serde")]
#[instrument]
#[test_log::test]
#[serial_test::parallel]
fn test_droptail_queue() {
    let mut config = RattanConfig::<StdPacket, StdNetEnvConfig> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
            ..Default::default()
        },
        ..Default::default()
    };
    config.cells.insert(
        "up_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::DropTail(BwCellConfig::new(
            None,
            DropTailQueueConfig::new(None, None, BwType::default()),
            None,
        ))),
    );
    config.cells.insert(
        "down_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::DropTail(BwCellConfig::new(
            None,
            DropTailQueueConfig::new(None, None, BwType::default()),
            None,
        ))),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver, StdNetEnv>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    {
        let _span = span!(Level::INFO, "run_test").entered();
        info!("Test DropTailQueue");

        let (msg_tx, msg_rx) = mpsc::channel();

        let cancel_token_inner = CancellationToken::new();
        let server_cancel_token = cancel_token_inner.clone();

        let right_handle = radix
            .right_spawn(None, || {
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
        let right_ip = <StdNetEnv as RattanEnv<AfPacketDriver>>::right_ip(&radix, 1).to_string();
        let left_handle = radix
            .left_spawn(None, move || {
                std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                sleep(Duration::from_millis(100));
                let client_socket = std::net::UdpSocket::bind("0.0.0.0:54321").unwrap();
                client_socket.connect(format!("{right_ip}:54321")).unwrap();

                info!("Test DropTailQueue (10 packets limit)");
                info!("Set bandwidth to 800kbps (1000B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigCell(
                            "up_bw".to_string(),
                            serde_json::to_value(
                                BwCellConfig::<StdPacket, DropTailQueue<StdPacket>>::new(
                                    Bandwidth::from_kbps(800),
                                    DropTailQueueConfig::new(10, None, BwType::default()),
                                    None,
                                ),
                            )
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
                let mut recv_indexes = Vec::new();
                while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
                    recv_indexes.push(index);
                }
                info!(?recv_indexes);
                assert!(recv_indexes.len() == 14);
                for (i, recv_index) in recv_indexes.iter().enumerate().take(12) {
                    assert!(*recv_index == i as u8);
                }
                assert!(19 <= recv_indexes[12] && recv_indexes[12] <= 20);
                assert!(27 <= recv_indexes[13] && recv_indexes[13] <= 30);

                info!("Test DropTailQueue (500 Bytes limit)");
                info!("Set bandwidth to 40kbps (50B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigCell(
                            "up_bw".to_string(),
                            serde_json::to_value(
                                BwCellConfig::<StdPacket, DropTailQueue<StdPacket>>::new(
                                    Bandwidth::from_kbps(40),
                                    DropTailQueueConfig::new(None, 500, BwType::default()),
                                    None,
                                ),
                            )
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
                let mut recv_indexes = Vec::new();
                while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
                    recv_indexes.push(index);
                }
                info!(?recv_indexes);
                assert!(recv_indexes.len() == 14);
                for (i, recv_index) in recv_indexes.iter().enumerate().take(12) {
                    assert!(*recv_index == i as u8);
                }
                assert!(19 <= recv_indexes[12] && recv_indexes[12] <= 20);
                assert!(27 <= recv_indexes[13] && recv_indexes[13] <= 30);
                Ok(())
            })
            .unwrap();

        left_handle.join().unwrap().unwrap();
        server_cancel_token.cancel();
        right_handle.join().unwrap().unwrap();
    }
}

#[cfg(feature = "serde")]
#[instrument]
#[test_log::test]
#[serial_test::parallel]
fn test_drophead_queue() {
    let mut config = RattanConfig::<StdPacket, StdNetEnvConfig> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
            ..Default::default()
        },
        ..Default::default()
    };
    config.cells.insert(
        "up_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::DropHead(BwCellConfig::new(
            None,
            DropHeadQueueConfig::new(None, None, BwType::default()),
            None,
        ))),
    );
    config.cells.insert(
        "down_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::DropHead(BwCellConfig::new(
            None,
            DropHeadQueueConfig::new(None, None, BwType::default()),
            None,
        ))),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix: RattanRadix<AfPacketDriver, StdNetEnv> =
        RattanRadix::<AfPacketDriver, StdNetEnv>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    {
        let _span = span!(Level::INFO, "run_test").entered();
        info!("Test DropHeadQueue");

        let (msg_tx, msg_rx) = mpsc::channel();

        let cancel_token_inner = CancellationToken::new();
        let server_cancel_token = cancel_token_inner.clone();

        let right_handle = radix
            .right_spawn(None, || {
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
        let right_ip = <StdNetEnv as RattanEnv<AfPacketDriver>>::right_ip(&radix, 1).to_string();
        let left_handle = radix
            .left_spawn(None, move || {
                std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                sleep(Duration::from_millis(100));
                let client_socket = std::net::UdpSocket::bind("0.0.0.0:54321").unwrap();
                client_socket.connect(format!("{right_ip}:54321")).unwrap();

                info!("Test DropHeadQueue (10 packets limit)");
                info!("Set bandwidth to 800kbps (1000B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigCell(
                            "up_bw".to_string(),
                            serde_json::to_value(
                                BwCellConfig::<StdPacket, DropHeadQueue<StdPacket>>::new(
                                    Bandwidth::from_kbps(800),
                                    DropHeadQueueConfig::new(10, None, BwType::default()),
                                    None,
                                ),
                            )
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
                let mut recv_indexes = Vec::new();
                while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
                    recv_indexes.push(index);
                }
                info!(?recv_indexes);
                assert!(recv_indexes.len() == 14);
                assert!(recv_indexes[0] == 0);
                assert!(recv_indexes[1] == 1);
                assert!(8 <= recv_indexes[2] && recv_indexes[2] <= 10);
                assert!(17 <= recv_indexes[3] && recv_indexes[3] <= 19);
                for (i, recv_index) in recv_indexes.iter().enumerate().take(14).skip(4) {
                    assert!(*recv_index == 16 + i as u8);
                }

                info!("Test DropHeadQueue (500 Bytes limit)");
                info!("Set bandwidth to 40kbps (50B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigCell(
                            "up_bw".to_string(),
                            serde_json::to_value(
                                BwCellConfig::<StdPacket, DropHeadQueue<StdPacket>>::new(
                                    Bandwidth::from_kbps(40),
                                    DropHeadQueueConfig::new(None, 500, BwType::default()),
                                    None,
                                ),
                            )
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
                let mut recv_indexes = Vec::new();
                while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
                    recv_indexes.push(index);
                }
                info!(?recv_indexes);
                assert!(recv_indexes.len() == 14);
                assert!(recv_indexes[0] == 0);
                assert!(recv_indexes[1] == 1);
                assert!(8 <= recv_indexes[2] && recv_indexes[2] <= 10);
                assert!(17 <= recv_indexes[3] && recv_indexes[3] <= 19);
                for (i, recv_index) in recv_indexes.iter().enumerate().take(14).skip(4) {
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

#[cfg(feature = "serde")]
#[instrument]
#[test_log::test]
#[serial_test::parallel]
fn test_codel_queue() {
    let mut config = RattanConfig::<StdPacket, StdNetEnvConfig> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
            ..Default::default()
        },
        ..Default::default()
    };
    config.cells.insert(
        "up_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::CoDel(BwCellConfig::new(
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
    config.cells.insert(
        "down_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::Infinite(BwCellConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver, StdNetEnv>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    {
        let _span = span!(Level::INFO, "run_test").entered();
        info!("Test CoDelQueue");

        let (msg_tx, msg_rx) = mpsc::channel();

        let cancel_token_inner = CancellationToken::new();
        let server_cancel_token = cancel_token_inner.clone();

        let right_handle = radix
            .right_spawn(None, || {
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
        let right_ip = <StdNetEnv as RattanEnv<AfPacketDriver>>::right_ip(&radix, 1).to_string();
        let left_handle = radix
            .left_spawn(None, move || {
                std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                sleep(Duration::from_millis(100));
                let client_socket = std::net::UdpSocket::bind("0.0.0.0:54321").unwrap();
                client_socket.connect(format!("{right_ip}:54321")).unwrap();

                info!("Test CoDelQueue (60 packets limit, target 50ms, interval 104ms, mtu 1500)");
                info!("Set bandwidth to 800kbps (1000B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigCell(
                            "up_bw".to_string(),
                            serde_json::to_value(
                                BwCellConfig::<StdPacket, CoDelQueue<StdPacket>>::new(
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
                let mut recv_indexes = Vec::new();
                while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
                    recv_indexes.push(index);
                }
                info!(?recv_indexes);
                let mut dropped_indexes = Vec::new();
                for i in 0..80 {
                    if !recv_indexes.contains(&(i as u8)) {
                        dropped_indexes.push(i as u8);
                    }
                }
                info!(?dropped_indexes);
                // A reference result: [18, 30, 38, 45, 51, 57, 62, 67, 72, 76, 77, 78] (len=12)
                // This result is same with mahimahi
                assert!(11 <= dropped_indexes.len() && dropped_indexes.len() <= 13);
                assert!(17 <= dropped_indexes[0] && dropped_indexes[0] <= 19);
                let pattern = [12, 8, 7, 6, 6, 5, 5, 4];
                for i in 0..pattern.len() {
                    assert!(
                        pattern[i] - 1 <= dropped_indexes[i + 1] - dropped_indexes[i]
                            && dropped_indexes[i + 1] - dropped_indexes[i] <= pattern[i] + 1
                    );
                }

                info!("Test CoDelQueue (500 Bytes limit, target 50ms, interval 104ms, mtu 80)");
                info!("Use last cycle as a good starting point");
                info!("Set bandwidth to 40kbps (50B per 10ms)");
                runtime
                    .block_on(
                        op_endpoint.exec(RattanOp::ConfigCell(
                            "up_bw".to_string(),
                            serde_json::to_value(
                                BwCellConfig::<StdPacket, CoDelQueue<StdPacket>>::new(
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
                let mut recv_indexes = Vec::new();
                while let Ok((index, _size, _timestamp)) = msg_rx.try_recv() {
                    recv_indexes.push(index);
                }
                info!(?recv_indexes);
                let mut dropped_indexes = Vec::new();
                for i in 0..80 {
                    if !recv_indexes.contains(&(i as u8)) {
                        dropped_indexes.push(i as u8);
                    }
                }
                info!(?dropped_indexes);
                // A reference result: [18, 23, 28, 32, 36, 40, 44, 48, 51, 55, 59, 62, 65, 69, 72, 76, 77, 78] (len=18)
                assert!(16 <= dropped_indexes.len() && dropped_indexes.len() <= 20);
                assert!(16 <= dropped_indexes[0] && dropped_indexes[0] <= 20);
                let pattern = [5, 5, 4, 4, 4, 4, 3, 4, 4, 3, 3, 4, 3];
                for i in 0..pattern.len() {
                    assert!(
                        pattern[i] - 1 <= dropped_indexes[i + 1] - dropped_indexes[i]
                            && dropped_indexes[i + 1] - dropped_indexes[i] <= pattern[i] + 1
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

/// Receives one datagram and returns its length and the TOS byte of its IPv4
/// header (requires `IP_RECVTOS` on the socket).
fn recv_with_tos(
    socket: &std::net::UdpSocket,
    buf: &mut [u8],
) -> std::io::Result<(usize, Option<u8>)> {
    use std::os::fd::AsRawFd;

    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let mut control = [0u64; 8];
    // SAFETY: `msg` points to `iov` and `control`, which outlive the calls below,
    // and the CMSG_* macros only walk the control buffer filled in by the kernel.
    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&control) as _;
        let len = libc::recvmsg(socket.as_raw_fd(), &mut msg, 0);
        if len < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut tos = None;
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::IPPROTO_IP && (*cmsg).cmsg_type == libc::IP_TOS {
                tos = Some(*libc::CMSG_DATA(cmsg));
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
        Ok((len as usize, tos))
    }
}

/// An L4S (ECT(1)) flow at half the link rate and a Classic (Not-ECT) flow at
/// the full link rate share a DualPI2 bottleneck.  The L4S flow must keep a low
/// delay and only be marked, while the Classic flow builds a queue and is
/// dropped.  As the L4S packets reach the receiver, their IPv4 checksum is also
/// correct after CE marking (the kernel would discard them otherwise).
#[instrument]
#[test_log::test]
#[serial_test::parallel]
fn test_dualpi2_queue() {
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        },
        time::Instant,
    };

    use nix::sys::socket::{setsockopt, sockopt};

    const CLASSIC: u8 = 0;
    const L4S: u8 = 1;
    const ECT1: i32 = 0b01;
    const TICKS: u32 = 1000;
    const TICK: Duration = Duration::from_millis(2); // 1000 B at 4 Mbps

    let temp = tempfile::tempdir().unwrap();
    let queue_log = temp.path().join("dualpi2.csv");
    let mut config = RattanConfig::<StdPacket, StdNetEnvConfig> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut up = BwCellConfig::new(
        Bandwidth::from_mbps(4),
        DualPi2QueueConfig {
            // Keep the unresponsive L4S flow loss-free: mark instead of
            // dropping on overload.
            drop_overload: false,
            ..Default::default()
        },
        None,
    );
    up.queue_log = Some(queue_log.clone());
    config.cells.insert(
        "up_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::DualPi2(up)),
    );
    config.cells.insert(
        "down_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::Infinite(BwCellConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver, StdNetEnv>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    let _span = span!(Level::INFO, "run_test").entered();
    info!("Test DualPi2Queue (4 Mbps, L4S at 2 Mbps + Classic at 4 Mbps, 1000 B packets)");

    let (msg_tx, msg_rx) = mpsc::channel();
    let (counts_tx, counts_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();

    let right_handle = radix
        .right_spawn(None, move || {
            let socket = std::net::UdpSocket::bind("0.0.0.0:54321").unwrap();
            setsockopt(&socket, sockopt::IpRecvTos, &true).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let mut buf = [0u8; 2048];
            while !server_stop.load(Ordering::Relaxed) {
                if let Ok((size, tos)) = recv_with_tos(&socket, &mut buf) {
                    let seq = u32::from_be_bytes(buf[1..5].try_into().unwrap());
                    msg_tx
                        .send((buf[0], seq, size, tos, Instant::now()))
                        .unwrap();
                }
            }
            Ok(())
        })
        .unwrap();

    let right_ip = <StdNetEnv as RattanEnv<AfPacketDriver>>::right_ip(&radix, 1);
    let left_handle = radix
        .left_spawn(None, move || {
            sleep(Duration::from_millis(100));
            let classic_socket = std::net::UdpSocket::bind("0.0.0.0:54322").unwrap();
            classic_socket.connect(format!("{right_ip}:54321")).unwrap();
            let l4s_socket = std::net::UdpSocket::bind("0.0.0.0:54323").unwrap();
            setsockopt(&l4s_socket, sockopt::Ipv4Tos, &ECT1).unwrap();
            l4s_socket.connect(format!("{right_ip}:54321")).unwrap();

            info!("Send Classic every 2 ms and L4S every 4 ms for 2 s");
            let mut send_times = [Vec::new(), Vec::new()];
            let mut payload = [0u8; 1000 - 28]; // 28 = 20(IPv4) + 8(UDP)
            let mut next_time = Instant::now();
            for tick in 0..TICKS {
                for (class, socket) in [(CLASSIC, &classic_socket), (L4S, &l4s_socket)] {
                    if class == L4S && tick % 2 == 1 {
                        continue;
                    }
                    let seq = send_times[class as usize].len() as u32;
                    payload[0] = class;
                    payload[1..5].copy_from_slice(&seq.to_be_bytes());
                    socket.send(&payload).unwrap();
                    send_times[class as usize].push(Instant::now());
                }
                next_time += TICK;
                sleep(next_time.saturating_duration_since(Instant::now()));
            }
            // Let the Classic queue drain.
            sleep(Duration::from_millis(2000));

            let mut delays = [Vec::new(), Vec::new()];
            let mut ce = [0, 0];
            while let Ok((class, seq, size, tos, received)) = msg_rx.try_recv() {
                assert_eq!(size, payload.len());
                let ecn = tos.expect("TOS should be reported") & 0b11;
                match class {
                    CLASSIC => assert_eq!(ecn, 0, "Classic packets are Not-ECT"),
                    L4S => assert_ne!(ecn, 0, "L4S packets stay ECN-capable"),
                    _ => panic!("unexpected class {class}"),
                }
                ce[class as usize] += usize::from(ecn == 0b11);
                delays[class as usize].push(received - send_times[class as usize][seq as usize]);
            }
            for d in delays.iter_mut() {
                d.sort();
            }
            let sent = [send_times[0].len(), send_times[1].len()];
            let received = [delays[0].len(), delays[1].len()];
            let median = |d: &Vec<Duration>| d[d.len() / 2];
            info!(?sent, ?received, ?ce);
            info!(
                classic_median = ?median(&delays[0]),
                l4s_median = ?median(&delays[1]),
                l4s_p99 = ?delays[1][delays[1].len() * 99 / 100],
            );

            // L4S: never dropped, mostly marked, low delay.
            assert_eq!(received[1], sent[1], "L4S packets must not be dropped");
            assert!(
                ce[1] * 10 >= received[1] * 8,
                "most L4S packets should be CE-marked: {}/{}",
                ce[1],
                received[1]
            );
            assert!(median(&delays[1]) < Duration::from_millis(15));

            // Classic: dropped with p'^2 <= 25% (p' is capped at 1/k), high delay.
            let classic_dropped = sent[0] - received[0];
            assert!(
                (sent[0] / 20..=sent[0] * 2 / 5).contains(&classic_dropped),
                "Classic drops: {classic_dropped}/{}",
                sent[0]
            );
            assert!(median(&delays[0]) > Duration::from_millis(100));
            assert!(median(&delays[0]) > 10 * median(&delays[1]));
            counts_tx.send((received, ce)).unwrap();
            Ok(())
        })
        .unwrap();

    left_handle.join().unwrap().unwrap();
    stop.store(true, Ordering::Relaxed);
    right_handle.join().unwrap().unwrap();
    drop(radix);

    let (received, ce) = counts_rx.recv().unwrap();
    let log = std::fs::read_to_string(queue_log).unwrap();
    let total = received.iter().sum::<usize>();
    assert_eq!(
        log.lines().last().unwrap(),
        format!("# complete records={total} dropped=0")
    );
    let mut reader = csv::ReaderBuilder::new()
        .comment(Some(b'#'))
        .from_reader(log.as_bytes());
    let mut recorded = [0, 0];
    let mut marked = [0, 0];
    let mut previous_start = i128::MIN;
    for row in reader.records() {
        let row = row.unwrap();
        assert_eq!(&row[2], "1000");
        assert_eq!(&row[4], "17");
        assert_eq!(&row[6], &right_ip.to_string());
        assert_eq!(&row[8], "54321");
        let class = match &row[7] {
            "54322" => CLASSIC as usize,
            "54323" => L4S as usize,
            port => panic!("unexpected source port {port}"),
        };
        let start: i128 = row[0].parse().unwrap();
        assert!(start >= previous_start);
        previous_start = start;
        let _: u64 = row[1].parse().unwrap();
        recorded[class] += 1;
        marked[class] += usize::from(&row[3] == "3");
    }
    assert_eq!(recorded, received);
    assert_eq!(marked, ce);
}

#[instrument]
#[test_log::test]
#[serial_test::serial]
fn test_replay() {
    let mut config = RattanConfig::<StdPacket, StdNetEnvConfig> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
            ..Default::default()
        },
        ..Default::default()
    };
    config.cells.insert(
        "up_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::Infinite(BwCellConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config
        .cells
        .insert("down_bw".to_string(), CellBuildConfig::Custom);
    config.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver, StdNetEnv>::new(config).unwrap();
    let control_interface = radix
        .build_cell("down_bw".to_string(), |handle| {
            let _guard = handle.enter();
            let trace = RepeatedBwPatternConfig::new()
                .pattern(vec![Box::new(StaticBwConfig {
                    bw: Some(Bandwidth::from_mbps(1)),
                    duration: Some(Duration::from_secs(10)),
                }) as Box<dyn BwTraceConfig>])
                .build();
            BwReplayCell::new(
                Box::new(trace) as Box<dyn BwTrace>,
                DropTailQueue::new(DropTailQueueConfig::new(100, None, BwType::default())).unwrap(),
                None,
            )
        })
        .unwrap();
    radix.spawn_rattan().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(100));
    {
        let _span = span!(Level::INFO, "test_replay").entered();
        let right_handle = radix
            .right_spawn(None, || {
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
                duration: Some(Duration::from_secs(6)),
            }),
            Box::new(StaticBwConfig {
                bw: Some(Bandwidth::from_mbps(20)),
                duration: Some(Duration::from_secs(6)),
            }) as Box<dyn BwTraceConfig>,
        ]);
        control_interface
            .set_config(BwReplayCellConfig::new(
                Box::new(trace_config) as Box<dyn BwTraceConfig>,
                None,
                None,
            ))
            .unwrap();
        let right_ip = <StdNetEnv as RattanEnv<AfPacketDriver>>::right_ip(&radix, 1).to_string();
        let left_handle = radix
            .left_spawn(None, move || {
                let client_handle = std::process::Command::new("iperf3")
                    .args([
                        "-c", &right_ip, "-p", "9000", "--cport", "10000", "-t", "12", "-J", "-R",
                        "-C", "reno",
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
            .take(11)
            .collect::<Vec<_>>();
        let front_5s_bandwidth = bandwidth.drain(0..5).collect::<Vec<_>>();

        // There could be a multi-hundred-ms gap between the initial request by iperf and the actual start point
        // of trasmission. Thus, skip one sec near the change point of bw.
        let _ = bandwidth.drain(0..1).take(1);
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

        // Correct for difference between payload length and L2 length
        let front_5s_target_rate = Bandwidth::from_mbps(100).as_bps() as f64 * 1460.0 / 1514.0;
        let back_5s_target_rate = Bandwidth::from_mbps(20).as_bps() as f64 * 1460.0 / 1514.0;

        let front_5s_bitrate = front_5s_bitrate as f64;
        let back_5s_bitrate = back_5s_bitrate as f64;

        assert!(front_5s_bitrate > front_5s_target_rate * 0.97);
        assert!(front_5s_bitrate < front_5s_target_rate * 1.03);
        assert!(back_5s_bitrate > back_5s_target_rate * 0.97);
        assert!(back_5s_bitrate < back_5s_target_rate * 1.03);
    }
}

#[instrument]
#[test_log::test]
#[serial_test::parallel]
fn test_low_rate() {
    // cargo run -- link --uplink-bandwidth 4096bps --ping -c 10 10.2.1.1 -s 100 -i 0.3
    let mut config = RattanConfig::<StdPacket, StdNetEnvConfig> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
            ..Default::default()
        },
        ..Default::default()
    };

    config.cells.insert(
        "up_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::Infinite(BwCellConfig::new(
            Bandwidth::from_bps(4096),
            InfiniteQueueConfig::new(),
            None,
        ))),
    );

    config.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver, StdNetEnv>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Wait for AfPacketDriver to be ready
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _span = span!(Level::INFO, "bandwidth_low_rate").entered();
    info!("try to ping 128B packets in a 4096bps link");
    let right_ip = <StdNetEnv as RattanEnv<AfPacketDriver>>::right_ip(&radix, 1).to_string();
    let left_handle = radix
        .left_spawn(None, move || {
            let handle = std::process::Command::new("ping")
                .args([&right_ip, "-c", "10", "-i", "0.3", "-s", "100"])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            Ok(handle.wait_with_output())
        })
        .unwrap();
    let output = left_handle.join().unwrap().unwrap().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    let re = Regex::new(r"time=(\d+)").unwrap();
    let latency = re
        .captures_iter(&stdout)
        .flat_map(|cap| cap[1].parse::<f64>())
        .collect::<Vec<_>>();
    info!(?latency);

    // Should all be 250ms.
    let (&min, &max) = latency.iter().minmax().into_option().unwrap();
    assert!(min >= 248.0);
    assert!(max <= 252.0);
}
