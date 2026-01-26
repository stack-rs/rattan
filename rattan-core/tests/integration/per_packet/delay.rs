/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test delay --all-features -- --nocapture
use netem_trace::{model::StaticDelayPerPacketConfig, Delay};
#[cfg(feature = "serde")]
use rattan_core::{cells::per_packet::delay::DelayPerPacketCellConfig, control::RattanOp};
use rattan_core::{
    cells::StdPacket,
    config::{CellBuildConfig, DelayPerPacketCellBuildConfig, RattanConfig},
    env::{StdNetEnvConfig, StdNetEnvMode},
    metal::io::af_packet::AfPacketDriver,
    radix::RattanRadix,
};
use regex::Regex;
use std::collections::HashMap;
use tracing::{info, instrument, span, Level};

#[instrument]
#[test_log::test]
#[serial_test::parallel]
fn test_delay() {
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
        "up_delay".to_string(),
        CellBuildConfig::DelayPerPacket(DelayPerPacketCellBuildConfig::Config(
            StaticDelayPerPacketConfig::new().delay(Delay::ZERO).into(),
        )),
    );
    config.cells.insert(
        "down_delay".to_string(),
        CellBuildConfig::DelayPerPacket(DelayPerPacketCellBuildConfig::Config(
            StaticDelayPerPacketConfig::new().delay(Delay::ZERO).into(),
        )),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_delay".to_string()),
        ("up_delay".to_string(), "right".to_string()),
        ("right".to_string(), "down_delay".to_string()),
        ("down_delay".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Wait for AfPacketDriver to be ready
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Before set the DelayCell, the average latency should be less than 0.1ms
    {
        let _span = span!(Level::INFO, "ping_no_delay").entered();
        info!("try to ping with no delay");
        let right_ip = radix.right_ip(1).to_string();
        let left_handle = radix
            .left_spawn(None, move |_user| {
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
        assert!(average_latency < 10.0);
    }

    // After set the DelayCell, the average latency should be around 200ms
    #[cfg(feature = "serde")]
    {
        let _span = span!(Level::INFO, "ping_with_delay").entered();
        info!(
            "try to ping with up delay set up delay to 100ms and down delay to 50ms (rtt = 150ms)"
        );
        radix
            .op_block_exec(RattanOp::ConfigCell(
                "up_delay".to_string(),
                serde_json::to_value(DelayPerPacketCellConfig::new(
                    StaticDelayPerPacketConfig::new().delay(Delay::from_millis(100)),
                ))
                .unwrap(),
            ))
            .unwrap();
        radix
            .op_block_exec(RattanOp::ConfigCell(
                "down_delay".to_string(),
                serde_json::to_value(DelayPerPacketCellConfig::new(
                    StaticDelayPerPacketConfig::new().delay(Delay::from_millis(50)),
                ))
                .unwrap(),
            ))
            .unwrap();

        let right_ip = radix.right_ip(1).to_string();
        let left_handle = radix
            .left_spawn(None, move |_| {
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
            .flat_map(|cap| cap[1].parse::<u64>())
            .collect::<Vec<_>>();
        info!(?latency);

        assert_eq!(latency.len(), 10);
        latency.drain(0..5);
        let average_latency = latency.iter().sum::<u64>() / latency.len() as u64;
        info!("average latency: {}", average_latency);
        assert!(average_latency > 145 && average_latency < 155);
    }
}
