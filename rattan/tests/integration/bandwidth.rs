/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test bandwidth --all-features -- --nocapture
use netem_trace::Bandwidth;
use rattan::core::{RattanMachine, RattanMachineConfig};
use rattan::devices::bandwidth::{BwDevice, BwDeviceConfig};
use rattan::devices::external::VirtualEthernet;
use rattan::devices::{ControlInterface, Device, StdPacket};
use rattan::env::{get_std_env, StdNetEnvConfig};
use rattan::metal::io::AfPacketDriver;
use rattan::metal::netns::NetNsGuard;
use regex::Regex;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::{error, info, instrument, span, warn, Instrument, Level};

#[instrument]
#[test_log::test]
fn test_bandwidth() {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();
    let right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let (control_tx, control_rx) = oneshot::channel();

    let rattan_thread_span = span!(Level::DEBUG, "rattan_thread").or_current();
    let rattan_thread = std::thread::spawn(move || {
        let _entered = rattan_thread_span.entered();
        let original_ns = _std_env.rattan_ns.enter().unwrap();
        let _left_pair_guard = _std_env.left_pair.clone();
        let _right_pair_guard = _std_env.right_pair.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(
            async move {
                let left_bw_device = BwDevice::<StdPacket>::new();
                let right_bw_device = BwDevice::<StdPacket>::new();
                let left_control_interface = left_bw_device.control_interface();
                let right_control_interface = right_bw_device.control_interface();
                if let Err(_) = control_tx.send((left_control_interface, right_control_interface)) {
                    error!("send control interface failed");
                }
                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.left_pair.right.clone(),
                );
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.right_pair.left.clone(),
                );

                let (left_bw_rx, left_bw_tx) = machine.add_device(left_bw_device);
                info!(left_bw_rx, left_bw_tx);
                let (right_bw_rx, right_bw_tx) = machine.add_device(right_bw_device);
                info!(right_bw_rx, right_bw_tx);
                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);

                machine.link_device(left_device_rx, left_bw_tx);
                machine.link_device(left_bw_rx, right_device_tx);
                machine.link_device(right_device_rx, right_bw_tx);
                machine.link_device(right_bw_rx, left_device_tx);

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8081,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    let (left_control_interface, right_control_interface) = control_rx.blocking_recv().unwrap();

    // Before set the BwDevice, the bandwidth should be around 1Gbps
    {
        let _span = span!(Level::INFO, "iperf_no_limit").entered();
        info!("try to iperf with no bandwidth limit");
        let handle = {
            let _right_ns_guard = NetNsGuard::new(right_ns.clone()).unwrap();
            std::thread::spawn(|| {
                let mut iperf_server = std::process::Command::new("iperf3")
                    .args(["-s", "-p", "9000", "-1"])
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                iperf_server.wait().unwrap();
            })
        };
        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        sleep(Duration::from_secs(1));
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
        let output = client_handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        handle.join().unwrap();
        if !stderr.is_empty() {
            warn!("{}", stderr);
        }
        let re = Regex::new(r#""bits_per_second":\s*(\d+)"#).unwrap();
        let bandwidth = re
            .captures_iter(&stdout)
            .flat_map(|cap| cap[1].parse::<u64>())
            .step_by(2)
            .take(10)
            .collect::<Vec<_>>();
        info!("bandwidth: {:?}", bandwidth);
        assert!(!bandwidth.is_empty());
    }
    // info!("====================================================");
    // After set the BwDevice, the bandwidth should be between 90-100Mbps
    std::thread::sleep(std::time::Duration::from_secs(1));
    {
        let _span = span!(Level::INFO, "iperf_with_limit").entered();
        info!("try to iperf with bandwidth limit set to 100Mbps");
        left_control_interface
            .set_config(BwDeviceConfig::new(Bandwidth::from_mbps(100)))
            .unwrap();
        right_control_interface
            .set_config(BwDeviceConfig::new(Bandwidth::from_mbps(100)))
            .unwrap();
        let handle = {
            let _right_ns_guard = NetNsGuard::new(right_ns.clone()).unwrap();
            std::thread::spawn(|| {
                let mut iperf_server = std::process::Command::new("iperf3")
                    .args(["-s", "-p", "9001", "-1"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                iperf_server.wait().unwrap();
            })
        };
        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        sleep(Duration::from_secs(1));
        let client_handle = std::process::Command::new("iperf3")
            .args([
                "-c",
                "192.168.12.1",
                "-p",
                "9001",
                "--cport",
                "10001",
                "-t",
                "10",
                "-J",
                "-R",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = client_handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        if !stderr.is_empty() {
            warn!("{}", stderr);
        }
        handle.join().unwrap();

        let re = Regex::new(r#""bits_per_second":\s*(\d+)"#).unwrap();
        let mut bandwidth = re
            .captures_iter(&stdout)
            .flat_map(|cap| cap[1].parse::<u64>())
            .step_by(2)
            .take(10)
            .collect::<Vec<_>>();

        bandwidth.drain(0..4);
        let bitrate = bandwidth.iter().sum::<u64>() / bandwidth.len() as u64;
        info!("bitrate: {}", bitrate);
        assert!(bitrate > 90000000 && bitrate < 100000000);
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}
