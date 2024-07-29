/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test compound --all-features -- --nocapture
use std::collections::HashMap;
use std::thread::sleep;
use std::time::Duration;

use netem_trace::Bandwidth;
use rattan_core::config::{
    BwDeviceBuildConfig, DelayDeviceBuildConfig, DeviceBuildConfig, LossDeviceBuildConfig,
    RattanConfig,
};
use rattan_core::control::RattanOp;
use rattan_core::devices::{
    bandwidth::{
        queue::{InfiniteQueue, InfiniteQueueConfig},
        BwDeviceConfig,
    },
    delay::DelayDeviceConfig,
    loss::LossDeviceConfig,
    StdPacket,
};
use rattan_core::env::{StdNetEnvConfig, StdNetEnvMode};
use rattan_core::metal::io::af_packet::AfPacketDriver;
use rattan_core::radix::RattanRadix;
use regex::Regex;
use tracing::{info, instrument, span, warn, Level};

#[instrument]
#[test_log::test]
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
    config.devices.insert(
        "up_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::Infinite(BwDeviceConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.devices.insert(
        "down_bw".to_string(),
        DeviceBuildConfig::Bw(BwDeviceBuildConfig::Infinite(BwDeviceConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.devices.insert(
        "up_delay".to_string(),
        DeviceBuildConfig::Delay(DelayDeviceBuildConfig::new(Duration::from_millis(0))),
    );
    config.devices.insert(
        "down_delay".to_string(),
        DeviceBuildConfig::Delay(DelayDeviceBuildConfig::new(Duration::from_millis(0))),
    );
    config.devices.insert(
        "up_loss".to_string(),
        DeviceBuildConfig::Loss(LossDeviceBuildConfig::new([])),
    );
    config.devices.insert(
        "down_loss".to_string(),
        DeviceBuildConfig::Loss(LossDeviceBuildConfig::new([])),
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

    // Before config the BwDevice, the bandwidth should be around 1Gbps
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

    // After set the BwDevice, the bandwidth should be between 80-100Mbps
    std::thread::sleep(std::time::Duration::from_millis(100));
    {
        let _span = span!(Level::INFO, "iperf_with_limit").entered();
        info!("try to iperf with bandwidth limit set to 100Mbps");
        radix
            .op_block_exec(RattanOp::ConfigDevice(
                "up_bw".to_string(),
                serde_json::to_value(BwDeviceConfig::<StdPacket, InfiniteQueue<StdPacket>>::new(
                    Bandwidth::from_mbps(100),
                    None,
                    None,
                ))
                .unwrap(),
            ))
            .unwrap();
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
        radix
            .op_block_exec(RattanOp::ConfigDevice(
                "up_delay".to_string(),
                serde_json::to_value(DelayDeviceConfig::new(Duration::from_millis(5))).unwrap(),
            ))
            .unwrap();
        radix
            .op_block_exec(RattanOp::ConfigDevice(
                "down_delay".to_string(),
                serde_json::to_value(DelayDeviceConfig::new(Duration::from_millis(10))).unwrap(),
            ))
            .unwrap();
        radix
            .op_block_exec(RattanOp::ConfigDevice(
                "up_loss".to_string(),
                serde_json::to_value(LossDeviceConfig::new(vec![0.001; 10])).unwrap(),
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
        assert!(bitrate > 80000000 && bitrate < 100000000);
    }
}
