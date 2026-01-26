use std::collections::HashMap;

use criterion::{criterion_group, criterion_main, Criterion};
use rattan_core::{
    cells::{
        bandwidth::{queue::InfiniteQueueConfig, BwCellConfig},
        StdPacket,
    },
    config::{BwCellBuildConfig, CellBuildConfig, RattanConfig},
    env::{StdNetEnvConfig, StdNetEnvMode},
    metal::io::af_packet::AfPacketDriver,
    radix::RattanRadix,
};

fn prepare_env() -> RattanRadix<AfPacketDriver> {
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
        "up_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::Infinite(BwCellConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.cells.insert(
        "down_bw".to_string(),
        CellBuildConfig::Bw(BwCellBuildConfig::Infinite(BwCellConfig::new(
            None,
            InfiniteQueueConfig::new(),
            None,
        ))),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_bw".to_string()),
        ("up_bw".to_string(), "right".to_string()),
        ("right".to_string(), "down_bw".to_string()),
        ("down_bw".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();
    radix
}

fn run_iperf(radix: &mut RattanRadix<AfPacketDriver>) {
    let right_handle = radix
        .right_spawn(None, |_| {
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
    let right_ip = radix.right_ip(1).to_string();
    let left_handle = radix
        .left_spawn(None, move |_| {
            std::process::Command::new("iperf3")
                .args([
                    "-c", &right_ip, "-p", "9000", "-n", "1024M", "-J", "-C", "reno",
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
