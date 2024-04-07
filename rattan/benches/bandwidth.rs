use std::collections::HashMap;

use criterion::{criterion_group, criterion_main, Criterion};
use rattan::{
    config::{BwDeviceBuildConfig, DeviceBuildConfig, RattanConfig},
    devices::{
        bandwidth::{queue::InfiniteQueueConfig, BwDeviceConfig},
        StdPacket,
    },
    env::{StdNetEnvConfig, StdNetEnvMode},
    radix::RattanRadix,
};

fn prepare_env() -> RattanRadix<StdPacket> {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
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
    config.core.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<StdPacket>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();
    radix
}

fn run_iperf(radix: &mut RattanRadix<StdPacket>) {
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

    std::thread::sleep(std::time::Duration::from_millis(100));
    let left_handle = radix
        .left_spawn(|| {
            std::process::Command::new("iperf3")
                .args([
                    "-c",
                    "192.168.12.1",
                    "-p",
                    "9000",
                    "-n",
                    "1024M",
                    "-J",
                    "-C",
                    "reno",
                ])
                .output()
                .unwrap();
            Ok(())
        })
        .unwrap();

    left_handle.join().unwrap().unwrap();
    right_handle.join().unwrap().unwrap();
}

fn criterion_benchmark(c: &mut Criterion) {
    let mut radix = prepare_env();

    let mut group = c.benchmark_group("Bandwidth");
    group
        .sample_size(10)
        .bench_function("af_packet", |b| b.iter(|| run_iperf(&mut radix)));

    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
