/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test token_bucket --all-features -- --nocapture
use std::collections::HashMap;

#[cfg(feature = "serde")]
use bandwidth::Bandwidth;
#[cfg(feature = "serde")]
use bytesize::ByteSize;
#[cfg(feature = "serde")]
use rattan_core::{cells::token_bucket::TokenBucketCellConfig, control::RattanOp};
use rattan_core::{
    cells::StdPacket,
    config::{CellBuildConfig, RattanConfig, TokenBucketCellBuildConfig},
    env::{StdNetEnvConfig, StdNetEnvMode},
    metal::io::af_packet::AfPacketDriver,
    radix::RattanRadix,
};
use regex::Regex;
use tracing::{info, instrument, span, Level};

#[instrument]
#[test_log::test]
#[serial_test::serial]
fn test_token_bucket() {
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
        "up_tb".to_string(),
        CellBuildConfig::TokenBucket(TokenBucketCellBuildConfig::new(
            None, None, None, None, None,
        )),
    );
    config.cells.insert(
        "down_tb".to_string(),
        CellBuildConfig::TokenBucket(TokenBucketCellBuildConfig::new(
            None, None, None, None, None,
        )),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_tb".to_string()),
        ("up_tb".to_string(), "right".to_string()),
        ("right".to_string(), "down_tb".to_string()),
        ("down_tb".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Wait for AfPacketDriver to be ready
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Before set the TokenBucketCell, the average latency should be less than 0.1ms
    {
        let _span = span!(Level::INFO, "ping_with_tb_unset").entered();
        info!("try to ping with token bucket unset");
        let right_ip = radix.right_ip(1).to_string();
        let left_handle = radix
            .left_spawn(None, move |_| {
                let handle = std::process::Command::new("ping")
                    .args([&right_ip, "-c", "10", "-i", "0.35", "-s", "256"])
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
        latency.drain(0..3);
        let average_latency = latency.iter().sum::<f64>() / latency.len() as f64;
        info!("average latency: {}", average_latency);
        assert!(average_latency < 10.0);
    }

    // After set the TokenBucketCell, the average latency should be around 500ms
    #[cfg(feature = "serde")]
    {
        let _span = span!(Level::INFO, "ping_with_tb_set").entered();
        info!("try to ping with the burst set to 256 B and the rate set to 4096 bps");
        radix
            .op_block_exec(RattanOp::ConfigCell(
                "up_tb".to_string(),
                serde_json::to_value(TokenBucketCellConfig::new(
                    None,
                    Bandwidth::from_bps(4096),
                    ByteSize::b(512),
                    None,
                    None,
                ))
                .unwrap(),
            ))
            .unwrap();

        let right_ip = radix.right_ip(1).to_string();
        let left_handle = radix
            .left_spawn(None, move |_| {
                let handle = std::process::Command::new("ping")
                    .args([&right_ip, "-c", "10", "-i", "0.35", "-s", "256"])
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
            .flat_map(|cap| cap[1].parse::<u64>())
            .collect::<Vec<_>>();
        info!(?latency);

        assert_eq!(latency.len(), 10);
        latency.drain(0..3);

        for window in latency.windows(2) {
            assert!(190 < window[1] - window[0] && window[1] - window[0] < 210);
        }
    }
}
