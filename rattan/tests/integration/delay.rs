/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test delay --all-features -- --nocapture
use rattan::core::{RattanMachine, RattanMachineConfig};
use rattan::devices::delay::{DelayDevice, DelayDeviceConfig};
use rattan::devices::external::VirtualEthernet;
use rattan::devices::{ControlInterface, Device, StdPacket};
use rattan::env::{get_std_env, StdNetEnvConfig};
use rattan::metal::io::AfPacketDriver;
use rattan::metal::netns::NetNsGuard;
use regex::Regex;
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::{error, info, instrument, span, Instrument, Level};

#[instrument]
#[test_log::test]
fn test_delay() {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();

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
                let left_delay_device = DelayDevice::<StdPacket>::new();
                let right_delay_device = DelayDevice::<StdPacket>::new();
                let left_control_interface = left_delay_device.control_interface();
                let right_control_interface = right_delay_device.control_interface();
                if let Err(_) = control_tx.send((left_control_interface, right_control_interface)) {
                    error!("send control interface failed");
                }
                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.left_pair.right.clone(),
                );
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.right_pair.left.clone(),
                );

                let (left_delay_rx, left_delay_tx) = machine.add_device(left_delay_device);
                info!(left_delay_rx, left_delay_tx);
                let (right_delay_rx, right_delay_tx) = machine.add_device(right_delay_device);
                info!(right_delay_rx, right_delay_tx);
                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);

                machine.link_device(left_device_rx, left_delay_tx);
                machine.link_device(left_delay_rx, right_device_tx);
                machine.link_device(right_device_rx, right_delay_tx);
                machine.link_device(right_delay_rx, left_device_tx);

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8082,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    // Wait for AfPacketDriver to be ready
    std::thread::sleep(std::time::Duration::from_millis(100));

    let (left_control_interface, right_control_interface) = control_rx.blocking_recv().unwrap();

    // Before set the DelayDevice, the average latency should be less than 0.1ms
    {
        let _span = span!(Level::INFO, "ping_no_delay").entered();
        info!("try to ping with no delay");
        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        let handle = std::process::Command::new("ping")
            .args(["192.168.12.1", "-c", "10", "-i", "0.3"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = handle.wait_with_output().unwrap();
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
    // info!("====================================================");
    // After set the DelayDevice, the average latency should be around 200ms
    {
        let _span = span!(Level::INFO, "ping_with_delay").entered();
        info!("try to ping with delay set to 100ms (rtt = 200ms)");
        left_control_interface
            .set_config(DelayDeviceConfig::new(Duration::from_millis(100)))
            .unwrap();
        right_control_interface
            .set_config(DelayDeviceConfig::new(Duration::from_millis(100)))
            .unwrap();
        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        let handle = std::process::Command::new("ping")
            .args(["192.168.12.1", "-c", "10", "-i", "0.3"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = handle.wait_with_output().unwrap();
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
        assert!(average_latency > 195 && average_latency < 205);
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}
