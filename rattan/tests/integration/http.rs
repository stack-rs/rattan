/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test http --all-features -- --nocapture
use std::collections::HashMap;
use std::thread::sleep;
use std::time::Duration;

use netem_trace::Bandwidth;
use rattan::config::{
    BwDeviceBuildConfig, DelayDeviceBuildConfig, DeviceBuildConfig, LossDeviceBuildConfig,
    RattanConfig,
};
use rattan::control::http::HttpConfig;
use rattan::devices::bandwidth::{
    queue::{InfiniteQueue, InfiniteQueueConfig},
    BwDeviceConfig,
};
use rattan::devices::delay::DelayDeviceConfig;
use rattan::devices::loss::LossDeviceConfig;
use rattan::devices::StdPacket;
use rattan::env::{StdNetEnvConfig, StdNetEnvMode};
use rattan::metal::io::af_packet::AfPacketDriver;
use rattan::radix::RattanRadix;
use regex::Regex;
use tracing::{error, info, instrument, span, warn, Level};

#[instrument]
#[test_log::test]
fn test_http() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
        },
        http: HttpConfig {
            enable: true,
            ..Default::default()
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
    config.core.devices.insert(
        "up_delay".to_string(),
        DeviceBuildConfig::Delay(DelayDeviceBuildConfig::new(Duration::from_millis(0))),
    );
    config.core.devices.insert(
        "down_delay".to_string(),
        DeviceBuildConfig::Delay(DelayDeviceBuildConfig::new(Duration::from_millis(0))),
    );
    config.core.devices.insert(
        "up_loss".to_string(),
        DeviceBuildConfig::Loss(LossDeviceBuildConfig::new([])),
    );
    config.core.devices.insert(
        "down_loss".to_string(),
        DeviceBuildConfig::Loss(LossDeviceBuildConfig::new([])),
    );
    config.core.links = HashMap::from([
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

    // After set the BwDevice, the bandwidth should be between 80-100Mbps
    std::thread::sleep(std::time::Duration::from_millis(100));
    {
        let _span = span!(Level::INFO, "iperf_with_limit").entered();
        info!("try to iperf with bandwidth limit set to 100Mbps");

        let client = reqwest::blocking::Client::new();
        // Wait for server up
        info!("wait for server up");
        loop {
            let mut err_count = 0;
            match client.get("http://127.0.0.1:8086/state").send() {
                Ok(response) => {
                    if response.status().is_success() {
                        info!("Rattan state: {}", response.text().unwrap());
                        break;
                    } else if err_count >= 10 {
                        error!("Get state failed 10 times: {}", response.status());
                    }
                }
                Err(err) => {
                    if err_count >= 10 {
                        error!("Get state failed 10 times: {}", err);
                    }
                }
            }
            err_count += 1;
            if err_count > 10 {
                panic!("Get state failed");
            }
            std::thread::sleep(Duration::from_millis(500));
        }

        // Test wrong http config
        let resp = client
            .post("http://127.0.0.1:8086/config/up_loss")
            .json(&BwDeviceConfig::<StdPacket, InfiniteQueue<StdPacket>>::new(
                Bandwidth::from_mbps(100),
                None,
                None,
            ))
            .send()
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
        info!(
            "Test wrong http config response: [{}] {}",
            resp.status(),
            resp.text().unwrap()
        );
        let resp = client
            .post("http://127.0.0.1:8086/config/up_bw")
            .json(&DelayDeviceConfig::new(Duration::from_millis(50)))
            .send()
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
        info!(
            "Test wrong http config response: [{}] {}",
            resp.status(),
            resp.text().unwrap()
        );

        // Test not found device
        let resp = client
            .post("http://127.0.0.1:8086/config/wrong_id")
            .json(&DelayDeviceConfig::new(Duration::from_millis(50)))
            .send()
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
        info!(
            "Test not found device response: [{}] {}",
            resp.status(),
            resp.text().unwrap()
        );

        // Test right http config
        let resp = client
            .post("http://127.0.0.1:8086/config/up_bw")
            .json(&BwDeviceConfig::<StdPacket, InfiniteQueue<StdPacket>>::new(
                Bandwidth::from_mbps(100),
                None,
                None,
            ))
            .send()
            .unwrap();
        assert!(resp.status().is_success());
        let resp = client
            .post("http://127.0.0.1:8086/config/down_bw")
            .json(&BwDeviceConfig::<StdPacket, InfiniteQueue<StdPacket>>::new(
                Bandwidth::from_mbps(100),
                None,
                None,
            ))
            .send()
            .unwrap();
        assert!(resp.status().is_success());
        let resp = client
            .post("http://127.0.0.1:8086/config/up_delay")
            .json(&DelayDeviceConfig::new(Duration::from_millis(10)))
            .send()
            .unwrap();
        assert!(resp.status().is_success());
        let resp = client
            .post("http://127.0.0.1:8086/config/down_delay")
            .json(&DelayDeviceConfig::new(Duration::from_millis(5)))
            .send()
            .unwrap();
        assert!(resp.status().is_success());
        let resp = client
            .post("http://127.0.0.1:8086/config/up_loss")
            .json(&LossDeviceConfig::new(vec![0.001; 10]))
            .send()
            .unwrap();
        assert!(resp.status().is_success());

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
