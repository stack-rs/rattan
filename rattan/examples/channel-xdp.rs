use bandwidth::Bandwidth;
use rattan::{
    config::{
        BwDeviceBuildConfig, DelayDeviceBuildConfig, DeviceBuildConfig, LossDeviceBuildConfig,
        RattanConfig,
    },
    devices::bandwidth::{queue::InfiniteQueueConfig, BwDeviceConfig},
    env::{StdNetEnvConfig, StdNetEnvMode},
    metal::io::af_xdp::{XDPDriver, XDPPacket},
    radix::RattanRadix,
};
use std::{collections::HashMap, thread::sleep, time::Duration};

use clap::Parser;

/// Search for a pattern in a file and display the lines that contain it.
#[derive(Parser)]
struct Cli {
    #[clap(long, action)]
    xdp: bool,
}

fn main() {
    let mut config = RattanConfig::<XDPPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![0],
            server_cores: vec![3, 7],
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

    config.core.resource.cpu = Some(vec![1, 2, 6]);

    let mut radix = RattanRadix::<XDPDriver>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Before config the BwDevice, the bandwidth should be around 1Gbps
    {
        let right_handle = radix
            .right_spawn(|| {
                let mut iperf_server = std::process::Command::new("taskset")
                    .args(["-c", "0", "iperf3", "-s", "-p", "9000", "-1"])
                    // .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                iperf_server.wait().unwrap();
                Ok(())
            })
            .unwrap();

        sleep(Duration::from_millis(500));

        let left_handle = radix
            .left_spawn(|| {
                let client_handle = std::process::Command::new("taskset")
                    .args([
                        "-c",
                        "3,7",
                        "iperf3",
                        "-c",
                        "192.168.12.1",
                        "-p",
                        "9000",
                        // "-P",
                        // "4",
                        "--cport",
                        "10000",
                        "-t",
                        "20",
                        "-J",
                        "-R",
                        "-C",
                        "bbr",
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
            eprintln!("{}", stderr);
        }

        let iperf_result: serde_json::Value = serde_json::from_str(&stdout).unwrap();

        let bandwidth = iperf_result["end"]["sum_received"]["bits_per_second"]
            .as_f64()
            .unwrap();

        println!(
            "bitrate: {:?}",
            Bandwidth::from_bps(bandwidth.round() as u64)
        );
    }
}
