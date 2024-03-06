use std::{sync::Arc, thread::JoinHandle};

use criterion::{criterion_group, criterion_main, Criterion};
use rattan::{
    core::{RattanMachine, RattanMachineConfig},
    devices::{
        bandwidth::{queue::InfiniteQueue, BwDevice, MAX_BANDWIDTH},
        external::VirtualEthernet,
        StdPacket,
    },
    env::{get_std_env, StdNetEnvConfig},
    metal::{io::AfPacketDriver, netns::NetNs},
};
use tokio_util::sync::CancellationToken;

fn prepare_env() -> (JoinHandle<()>, CancellationToken, Arc<NetNs>, Arc<NetNs>) {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();
    let right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let rattan_thread = std::thread::spawn(move || {
        let original_ns = _std_env.rattan_ns.enter().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async move {
            let left_bw_device = BwDevice::new(MAX_BANDWIDTH, InfiniteQueue::new());
            let right_bw_device = BwDevice::new(MAX_BANDWIDTH, InfiniteQueue::new());
            let left_device =
                VirtualEthernet::<StdPacket, AfPacketDriver>::new(_std_env.left_pair.right.clone());
            let right_device =
                VirtualEthernet::<StdPacket, AfPacketDriver>::new(_std_env.right_pair.left.clone());

            let (left_bw_rx, left_bw_tx) = machine.add_device(left_bw_device);
            let (right_bw_rx, right_bw_tx) = machine.add_device(right_bw_device);
            let (left_device_rx, left_device_tx) = machine.add_device(left_device);
            let (right_device_rx, right_device_tx) = machine.add_device(right_device);

            machine.link_device(left_device_rx, left_bw_tx);
            machine.link_device(left_bw_rx, right_device_tx);
            machine.link_device(right_device_rx, right_bw_tx);
            machine.link_device(right_bw_rx, left_device_tx);

            let config = RattanMachineConfig {
                original_ns,
                port: 8080,
            };
            machine.core_loop(config).await
        });
    });

    (rattan_thread, cancel_token, left_ns, right_ns)
}

fn run_iperf(left_ns: &Arc<NetNs>, right_ns: &Arc<NetNs>) {
    let left_ns = left_ns.clone();
    let right_ns = right_ns.clone();
    let handle = {
        std::thread::spawn(move || {
            right_ns.enter().unwrap();
            let mut iperf_server = std::process::Command::new("iperf3")
                .args(["-s", "-p", "9000", "-1"])
                .spawn()
                .unwrap();
            iperf_server.wait().unwrap();
        })
    };

    {
        left_ns.enter().unwrap();
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
    }

    handle.join().unwrap();
}

fn criterion_benchmark(c: &mut Criterion) {
    let (rattan_thread, cancel_token, left_ns, right_ns) = prepare_env();

    let mut group = c.benchmark_group("Bandwidth");
    group
        .sample_size(10)
        .bench_function("af_packet", |b| b.iter(|| run_iperf(&left_ns, &right_ns)));

    group.finish();
    cancel_token.cancel();
    rattan_thread.join().unwrap();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
