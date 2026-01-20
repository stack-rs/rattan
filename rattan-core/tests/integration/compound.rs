/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test compound --all-features -- --nocapture
use std::{collections::HashMap, thread::sleep, time::Duration};

use netem_trace::Bandwidth;
#[cfg(feature = "serde")]
use rattan_core::{
    cells::{bandwidth::queue::InfiniteQueue, delay::DelayCellConfig, loss::LossCellConfig},
    control::RattanOp,
};
use rattan_core::{
    cells::{
        bandwidth::{queue::InfiniteQueueConfig, BwCellConfig},
        StdPacket,
    },
    config::{
        BwCellBuildConfig, CellBuildConfig, DelayCellBuildConfig, LossCellBuildConfig, RattanConfig,
    },
    env::{StdNetEnvConfig, StdNetEnvMode},
    metal::io::af_packet::AfPacketDriver,
    radix::RattanRadix,
};
use regex::Regex;
use tracing::{info, instrument, span, warn, Level};

#[instrument]
#[test_log::test]
#[serial_test::serial]
fn test_compound() {
    let mut config = RattanConfig::<StdPacket> {
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
    config.cells.insert(
        "up_delay".to_string(),
        CellBuildConfig::Delay(DelayCellBuildConfig::new(Duration::from_millis(0))),
    );
    config.cells.insert(
        "down_delay".to_string(),
        CellBuildConfig::Delay(DelayCellBuildConfig::new(Duration::from_millis(0))),
    );
    config.cells.insert(
        "up_loss".to_string(),
        CellBuildConfig::Loss(LossCellBuildConfig::new([])),
    );
    config.cells.insert(
        "down_loss".to_string(),
        CellBuildConfig::Loss(LossCellBuildConfig::new([])),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "up_delay".to_string()),
        ("up_delay".to_string(), "up_loss".to_string()),
        ("up_loss".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "down_delay".to_string()),
        ("down_delay".to_string(), "down_loss".to_string()),
        ("down_loss".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver>::new(config).unwrap();
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
        let right_ip = radix.right_ip(1).to_string();
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

    // After set the BwCell, the bandwidth should be between 80-100Mbps
    std::thread::sleep(std::time::Duration::from_millis(100));
    #[cfg(feature = "serde")]
    {
        let _span = span!(Level::INFO, "iperf_with_limit").entered();
        info!("try to iperf with bandwidth limit set to 100Mbps");
        radix
            .op_block_exec(RattanOp::ConfigCell(
                "up_bw".to_string(),
                serde_json::to_value(BwCellConfig::<StdPacket, InfiniteQueue<StdPacket>>::new(
                    Bandwidth::from_mbps(100),
                    None,
                    None,
                ))
                .unwrap(),
            ))
            .unwrap();
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
        radix
            .op_block_exec(RattanOp::ConfigCell(
                "up_delay".to_string(),
                serde_json::to_value(DelayCellConfig::new(Duration::from_millis(5))).unwrap(),
            ))
            .unwrap();
        radix
            .op_block_exec(RattanOp::ConfigCell(
                "down_delay".to_string(),
                serde_json::to_value(DelayCellConfig::new(Duration::from_millis(10))).unwrap(),
            ))
            .unwrap();
        radix
            .op_block_exec(RattanOp::ConfigCell(
                "up_loss".to_string(),
                serde_json::to_value(LossCellConfig::new(vec![0.001; 10])).unwrap(),
            ))
            .unwrap();

        sleep(Duration::from_millis(500));
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
        let right_ip = radix.right_ip(1).to_string();
        let left_handle = radix
            .left_spawn(None, move || {
                let client_handle = std::process::Command::new("iperf3")
                    .args([
                        "-c", &right_ip, "-p", "9001", "--cport", "10000", "-t", "10", "-J", "-R",
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
        assert!(
            bitrate > 80_000_000 && bitrate < 100_000_000,
            "The bitrate is {bitrate} and should be between 80000000 and 100000000"
        );
    }
}
