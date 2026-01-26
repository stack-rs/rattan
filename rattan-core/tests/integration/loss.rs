/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test loss --all-features -- --nocapture
use std::collections::HashMap;

#[cfg(feature = "serde")]
use rattan_core::{cells::loss::LossCellConfig, control::RattanOp};
use rattan_core::{
    cells::StdPacket,
    config::{CellBuildConfig, LossCellBuildConfig, RattanConfig},
    env::{StdNetEnvConfig, StdNetEnvMode},
    metal::io::af_packet::AfPacketDriver,
    radix::RattanRadix,
};
use regex::Regex;
use tracing::{info, instrument, span, Level};

#[instrument]
#[test_log::test]
#[serial_test::parallel]
fn test_loss() {
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
        "up_loss".to_string(),
        CellBuildConfig::Loss(LossCellBuildConfig::new([])),
    );
    config.cells.insert(
        "down_loss".to_string(),
        CellBuildConfig::Loss(LossCellBuildConfig::new([])),
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

    // Before set the LossCell, the average loss rate should be 0%
    {
        let _span = span!(Level::INFO, "ping_no_loss").entered();
        info!("try to ping with no loss");
        let right_ip = radix.right_ip(1).to_string();
        let left_handle = radix
            .left_spawn(None, move |_| {
                let handle = std::process::Command::new("ping")
                    .args([&right_ip, "-c", "20", "-i", "0.3"])
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

    // After set the LossCell, the average loss rate should be between 40%-60%
    #[cfg(feature = "serde")]
    {
        let _span = span!(Level::INFO, "ping_with_loss").entered();
        info!("try to ping with loss set to 0.5");
        radix
            .op_block_exec(RattanOp::ConfigCell(
                "up_loss".to_string(),
                serde_json::to_value(LossCellConfig::new(vec![0.5; 10])).unwrap(),
            ))
            .unwrap();

        let right_ip = radix.right_ip(1).to_string();
        let left_handle = radix
            .left_spawn(None, move |_| {
                let handle = std::process::Command::new("ping")
                    .args([&right_ip, "-c", "50", "-i", "0.3"])
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
