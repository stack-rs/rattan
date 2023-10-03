/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test http --all-features -- --nocapture
use netem_trace::Bandwidth;
use rand::rngs::StdRng;
use rand::SeedableRng;
use rattan::core::{RattanMachine, RattanMachineConfig};
use rattan::devices::bandwidth::{BwDevice, BwDeviceConfig};
use rattan::devices::delay::{DelayDevice, DelayDeviceConfig};
use rattan::devices::loss::{LossDevice, LossDeviceConfig};

use rattan::devices::external::VirtualEthernet;
use rattan::devices::StdPacket;
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
fn test_http() {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();
    let right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let (delay_control_tx, delay_control_rx) = oneshot::channel();
    let (loss_control_tx, loss_control_rx) = oneshot::channel();
    let (bw_control_tx, bw_control_rx) = oneshot::channel();

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
                let rng = StdRng::seed_from_u64(42);
                let left_bw_device = BwDevice::<StdPacket>::new();
                let right_bw_device = BwDevice::<StdPacket>::new();
                let left_delay_device = DelayDevice::<StdPacket>::new();
                let right_delay_device = DelayDevice::<StdPacket>::new();
                let left_loss_device = LossDevice::<StdPacket, StdRng>::new(rng.clone());
                let right_loss_device = LossDevice::<StdPacket, StdRng>::new(rng);

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
                if let Err(_) = bw_control_tx.send((left_bw_tx, right_bw_tx)) {
                    error!("send control interface failed");
                }
                let (left_delay_rx, left_delay_tx) = machine.add_device(left_delay_device);
                info!(left_delay_rx, left_delay_tx);
                let (right_delay_rx, right_delay_tx) = machine.add_device(right_delay_device);
                info!(right_delay_rx, right_delay_tx);
                if let Err(_) = delay_control_tx.send((left_delay_tx, right_delay_tx)) {
                    error!("send control interface failed");
                }
                let (left_loss_rx, left_loss_tx) = machine.add_device(left_loss_device);
                info!(left_loss_rx, left_loss_tx);
                let (right_loss_rx, right_loss_tx) = machine.add_device(right_loss_device);
                info!(right_loss_rx, right_loss_tx);
                if let Err(_) = loss_control_tx.send((left_loss_tx, right_loss_tx)) {
                    error!("send control interface failed");
                }
                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);

                machine.link_device(left_device_rx, left_bw_tx);
                machine.link_device(left_bw_rx, left_delay_tx);
                machine.link_device(left_delay_rx, left_loss_tx);
                machine.link_device(left_loss_rx, right_device_tx);

                machine.link_device(right_device_rx, right_bw_tx);
                machine.link_device(right_bw_rx, right_delay_tx);
                machine.link_device(right_delay_rx, right_loss_tx);
                machine.link_device(right_loss_rx, left_device_tx);

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8087,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    let (left_bw_ctl, right_bw_ctl) = bw_control_rx.blocking_recv().unwrap();
    let (left_delay_ctl, right_delay_ctl) = delay_control_rx.blocking_recv().unwrap();
    let (left_loss_ctl, _) = loss_control_rx.blocking_recv().unwrap();

    // Before set the BwDevice, the bandwidth should be around 1Gbps
    {
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
        if stderr.len() > 0 {
            warn!("{}", stderr);
        }
        let re = Regex::new(r#""bits_per_second":\s*(\d+)"#).unwrap();
        let bandwidth = re
            .captures_iter(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .flatten()
            .step_by(2)
            .take(10)
            .collect::<Vec<_>>();
        info!("bandwidth: {:?}", bandwidth);
        assert!(bandwidth.len() > 0);
    }
    // info!("====================================================");
    // After set the BwDevice, the bandwidth should be between 90-100Mbps
    std::thread::sleep(std::time::Duration::from_secs(1));
    {
        info!("try to iperf with bandwidth limit set to 100Mbps");
        let client = reqwest::blocking::Client::new();
        // Test wrong http config
        let resp = client
            .post(format!("http://127.0.0.1:8087/control/{}", left_loss_ctl))
            .json(&BwDeviceConfig::new(Bandwidth::from_mbps(100)))
            .send()
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
        let resp = client
            .post(format!("http://127.0.0.1:8087/control/{}", left_bw_ctl))
            .json(&DelayDeviceConfig::new(Duration::from_millis(50)))
            .send()
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

        // Test not found config endpoint
        let resp = client
            .post(format!(
                "http://127.0.0.1:8087/control/{}",
                left_bw_ctl + 10000
            ))
            .json(&DelayDeviceConfig::new(Duration::from_millis(50)))
            .send()
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

        // Test right http config
        let resp = client
            .post(format!("http://127.0.0.1:8087/control/{}", left_bw_ctl))
            .json(&BwDeviceConfig::new(Bandwidth::from_mbps(100)))
            .send()
            .unwrap();
        assert!(resp.status().is_success());
        let resp = client
            .post(format!("http://127.0.0.1:8087/control/{}", right_bw_ctl))
            .json(&BwDeviceConfig::new(Bandwidth::from_mbps(100)))
            .send()
            .unwrap();
        assert!(resp.status().is_success());
        let resp = client
            .post(format!("http://127.0.0.1:8087/control/{}", left_delay_ctl))
            .json(&DelayDeviceConfig::new(Duration::from_millis(50)))
            .send()
            .unwrap();
        assert!(resp.status().is_success());
        let resp = client
            .post(format!("http://127.0.0.1:8087/control/{}", right_delay_ctl))
            .json(&DelayDeviceConfig::new(Duration::from_millis(50)))
            .send()
            .unwrap();
        assert!(resp.status().is_success());
        let resp = client
            .post(format!("http://127.0.0.1:8087/control/{}", left_loss_ctl))
            .json(&LossDeviceConfig::new(vec![0.01; 10]))
            .send()
            .unwrap();
        assert!(resp.status().is_success());

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
        if stderr.len() > 0 {
            warn!("{}", stderr);
        }
        handle.join().unwrap();

        let re = Regex::new(r#""bits_per_second":\s*(\d+)"#).unwrap();
        let mut bandwidth = re
            .captures_iter(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .flatten()
            .step_by(2)
            .take(10)
            .collect::<Vec<_>>();

        bandwidth.drain(0..4);
        let bitrate = bandwidth.iter().sum::<u64>() / bandwidth.len() as u64;
        info!("bitrate: {}", bitrate);
        // assert!(bitrate > 90000000 && bitrate < 100000000);
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}
