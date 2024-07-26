/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test loss --all-features -- --nocapture
use std::collections::HashMap;

use rattan_core::config::{DeviceBuildConfig, LossDeviceBuildConfig, RattanConfig};
use rattan_core::control::RattanOp;
use rattan_core::devices::{loss::LossDeviceConfig, StdPacket};
use rattan_core::env::{StdNetEnvConfig, StdNetEnvMode};
use rattan_core::metal::io::af_packet::AfPacketDriver;
use rattan_core::radix::RattanRadix;
use regex::Regex;
use tracing::{info, instrument, span, Level};

#[instrument]
#[test_log::test]
fn test_loss() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
        },
        ..Default::default()
    };
    config.devices.insert(
        "up_loss".to_string(),
        DeviceBuildConfig::Loss(LossDeviceBuildConfig::new([])),
    );
    config.devices.insert(
        "down_loss".to_string(),
        DeviceBuildConfig::Loss(LossDeviceBuildConfig::new([])),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_loss".to_string()),
        ("up_loss".to_string(), "right".to_string()),
        ("right".to_string(), "down_loss".to_string()),
        ("down_loss".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Wait for AfPacketDriver to be ready
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Before set the LossDevice, the average loss rate should be 0%
    {
        let _span = span!(Level::INFO, "ping_no_loss").entered();
        info!("try to ping with no loss");
        let left_handle = radix
            .left_spawn(None, || {
                let handle = std::process::Command::new("ping")
                    .args(["192.168.12.1", "-c", "20", "-i", "0.3"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                Ok(handle.wait_with_output())
            })
            .unwrap();
        let output = left_handle.join().unwrap().unwrap().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"(\d+)% packet loss").unwrap();
        let loss_percentage = re
            .captures(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .unwrap()
            .unwrap();
        info!("loss_percentage: {}", loss_percentage);
        assert!(loss_percentage == 0);
    }

    // After set the LossDevice, the average loss rate should be between 40%-60%
    {
        let _span = span!(Level::INFO, "ping_with_loss").entered();
        info!("try to ping with loss set to 0.5");
        radix
            .op_block_exec(RattanOp::ConfigDevice(
                "up_loss".to_string(),
                serde_json::to_value(LossDeviceConfig::new(vec![0.5; 10])).unwrap(),
            ))
            .unwrap();

        let left_handle = radix
            .left_spawn(None, || {
                let handle = std::process::Command::new("ping")
                    .args(["192.168.12.1", "-c", "50", "-i", "0.3"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                Ok(handle.wait_with_output())
            })
            .unwrap();
        let output = left_handle.join().unwrap().unwrap().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"(\d+)% packet loss").unwrap();
        let loss_percentage = re
            .captures(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .unwrap()
            .unwrap();
        info!("loss_percentage: {}", loss_percentage);
        assert!((40..=60).contains(&loss_percentage));
    }
}
