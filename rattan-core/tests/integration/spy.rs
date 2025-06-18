/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test spy --all-features -- --nocapture
use rattan_core::cells::StdPacket;
use rattan_core::config::{CellBuildConfig, RattanConfig, SpyCellBuildConfig};
use rattan_core::env::{StdNetEnvConfig, StdNetEnvMode};
use rattan_core::metal::io::af_packet::AfPacketDriver;
use rattan_core::radix::RattanRadix;
use regex::Regex;
use std::collections::HashMap;
use std::io::Cursor;
use tempfile::NamedTempFile;
use tracing::{info, instrument, span, Level};

#[instrument]
#[test_log::test]
fn test_spy() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
            ..Default::default()
        },
        ..Default::default()
    };
    let file = NamedTempFile::new().unwrap();
    config.cells.insert(
        "up_spy".to_string(),
        CellBuildConfig::Spy(SpyCellBuildConfig::Path(file.path().to_path_buf())),
    );
    config.cells.insert(
        "down_spy".to_string(),
        CellBuildConfig::Spy(SpyCellBuildConfig::Config(Box::new(
            Cursor::new(Vec::new()),
        ))),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_spy".to_string()),
        ("up_spy".to_string(), "right".to_string()),
        ("right".to_string(), "down_spy".to_string()),
        ("down_spy".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Wait for AfPacketDriver to be ready
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Through the SpyCell, the average latency should be less than 0.1ms
    {
        let _span = span!(Level::INFO, "ping_no_spy").entered();
        info!("try to ping with no spy");
        let right_ip = radix.right_ip(1).to_string();
        let left_handle = radix
            .left_spawn(None, move || {
                let handle = std::process::Command::new("ping")
                    .args([&right_ip, "-c", "10", "-i", "0.3"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                Ok(handle.wait_with_output())
            })
            .unwrap();
        let output = left_handle.join().unwrap().unwrap().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"time=(\d+)").unwrap();
        let mut latency = re
            .captures_iter(&stdout)
            .flat_map(|cap| cap[1].parse::<f64>())
            .collect::<Vec<_>>();
        info!(?latency);

        assert_eq!(latency.len(), 10);
        latency.drain(0..5);
        let average_latency = latency.iter().sum::<f64>() / latency.len() as f64;
        info!("average latency: {}", average_latency);
        assert!(average_latency <= 0.1);
    }
}
