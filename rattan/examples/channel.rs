use bandwidth::Bandwidth;
use rattan::{
    config::{
        BwDeviceBuildConfig, DelayDeviceBuildConfig, DeviceBuildConfig, LossDeviceBuildConfig,
        RattanConfig,
    },
    devices::{
        bandwidth::{queue::InfiniteQueueConfig, BwDeviceConfig},
        StdPacket,
    },
    env::{StdNetEnvConfig, StdNetEnvMode},
    metal::io::af_packet::AfPacketDriver,
    radix::RattanRadix,
};
use regex::Regex;
use std::{collections::HashMap, thread::sleep, time::Duration};

use clap::Parser;

/// Search for a pattern in a file and display the lines that contain it.
#[derive(Parser)]
struct Cli {
    #[clap(long, action)]
    xdp: bool,
}

fn main() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
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

    config.resource.cpu = Some(vec![2]);

    let mut radix = RattanRadix::<AfPacketDriver>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Before config the BwDevice, the bandwidth should be around 1Gbps
    {
        let right_handle = radix
            .right_spawn(|| {
                let mut iperf_server = std::process::Command::new("taskset")
                    .args(["-c", "1", "iperf3", "-s", "-p", "9000", "-1"])
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
                let client_handle = std::process::Command::new("taskset")
                    .args([
                        "-c",
                        "3",
                        "iperf3",
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

        let re = Regex::new(r#""bits_per_second":\s*(\d+)"#).unwrap();
        let mut bandwidth = re
            .captures_iter(&stdout)
            .flat_map(|cap| cap[1].parse::<u64>())
            .step_by(2)
            .take(10)
            .collect::<Vec<_>>();

        assert!(!bandwidth.is_empty());

        bandwidth.drain(0..4);
        let bitrate = bandwidth.iter().sum::<u64>() / bandwidth.len() as u64;
        println!("bitrate: {:?}", Bandwidth::from_bps(bitrate));
    }
}
