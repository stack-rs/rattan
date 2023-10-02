/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test loss --all-features -- --nocapture
use rand::rngs::StdRng;
use rand::SeedableRng;
use rattan::core::{RattanMachine, RattanMachineConfig};
use rattan::devices::external::VirtualEthernet;
use rattan::devices::loss::{IIDLossDevice, IIDLossDeviceConfig, LossDevice, LossDeviceConfig};
use rattan::devices::{ControlInterface, Device, StdPacket};
use rattan::env::{get_std_env, StdNetEnvConfig};
use rattan::metal::io::AfPacketDriver;
use rattan::metal::netns::NetNsGuard;
use regex::Regex;
use tokio::sync::oneshot;
use tracing::{error, info, instrument, span, Instrument, Level};

#[instrument]
#[test_log::test]
fn test_loss_pattern() {
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
                let rng = StdRng::seed_from_u64(42);
                let left_loss_device = LossDevice::<StdPacket, StdRng>::new(rng.clone());
                let right_loss_device = LossDevice::<StdPacket, StdRng>::new(rng);
                let left_control_interface = left_loss_device.control_interface();
                let right_control_interface = right_loss_device.control_interface();
                if let Err(_) = control_tx.send((left_control_interface, right_control_interface)) {
                    error!("send control interface failed");
                }
                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.left_pair.right.clone(),
                );
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.right_pair.left.clone(),
                );

                let (left_loss_rx, left_loss_tx) = machine.add_device(left_loss_device);
                info!(left_loss_rx, left_loss_tx);
                let (right_loss_rx, right_loss_tx) = machine.add_device(right_loss_device);
                info!(right_loss_rx, right_loss_tx);
                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);

                machine.link_device(left_device_rx, left_loss_tx);
                machine.link_device(left_loss_rx, right_device_tx);
                machine.link_device(right_device_rx, right_loss_tx);
                machine.link_device(right_loss_rx, left_device_tx);

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8083,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    // Wait for AfPacketDriver to be ready
    std::thread::sleep(std::time::Duration::from_millis(100));

    let (left_control_interface, _) = control_rx.blocking_recv().unwrap();

    // Before set the LossDevice, the average loss rate should be 0%
    {
        let _span = span!(Level::INFO, "ping_no_loss").entered();
        info!("try to ping with no loss");
        left_ns.enter().unwrap();
        let handle = std::process::Command::new("ping")
            .args(["192.168.12.1", "-c", "10", "-i", "0.3"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"(\d+)% packet loss").unwrap();
        let loss_percentage = re
            .captures(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .unwrap()
            .unwrap();
        info!("loss_percentage: {}", loss_percentage);
        assert!(loss_percentage == 0);
    }
    // info!("====================================================");
    // After set the LossDevice, the average loss rate should be between 40%-60%
    {
        let _span = span!(Level::INFO, "ping_with_loss").entered();
        info!("try to ping with loss set to 0.5");
        left_control_interface
            .set_config(LossDeviceConfig::new(vec![0.5; 10]))
            .unwrap();
        left_ns.enter().unwrap();
        let handle = std::process::Command::new("ping")
            .args(["192.168.12.1", "-c", "50", "-i", "0.3"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"(\d+)% packet loss").unwrap();
        let loss_percentage = re
            .captures(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .unwrap()
            .unwrap();
        info!("loss_percentage: {}", loss_percentage);
        assert!(loss_percentage >= 40 && loss_percentage <= 60);
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}

// cargo test --package rattan --test main --features http -- integration::loss::test_iid_loss --exact --nocapture
#[instrument]
#[test_log::test]
fn test_iid_loss() {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let (control_tx, control_rx) = oneshot::channel();

    let rattan_thread_span = span!(Level::INFO, "rattan_thread").or_current();
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
                let left_loss_device = IIDLossDevice::<StdPacket, StdRng>::new(rng.clone());
                let right_loss_device = IIDLossDevice::<StdPacket, StdRng>::new(rng);
                let left_control_interface = left_loss_device.control_interface();
                let right_control_interface = right_loss_device.control_interface();
                if let Err(_) = control_tx.send((left_control_interface, right_control_interface)) {
                    error!("send control interface failed");
                }
                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.left_pair.right.clone(),
                );
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.right_pair.left.clone(),
                );

                let (left_loss_rx, left_loss_tx) = machine.add_device(left_loss_device);
                info!(left_loss_rx, left_loss_tx);
                let (right_loss_rx, right_loss_tx) = machine.add_device(right_loss_device);
                info!(right_loss_rx, right_loss_tx);
                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);

                machine.link_device(left_device_rx, left_loss_tx);
                machine.link_device(left_loss_rx, right_device_tx);
                machine.link_device(right_device_rx, right_loss_tx);
                machine.link_device(right_loss_rx, left_device_tx);

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8084,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    // Wait for AfPacketDriver to be ready
    std::thread::sleep(std::time::Duration::from_millis(100));

    let (left_control_interface, _) = control_rx.blocking_recv().unwrap();

    // Before set the IIDLossDevice, the average loss rate should be 0%
    {
        let _span = span!(Level::INFO, "ping_no_loss").entered();
        info!("try to ping with no loss");
        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        let handle = std::process::Command::new("ping")
            .args(["192.168.12.1", "-c", "10", "-i", "0.3"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"(\d+)% packet loss").unwrap();
        let loss_percentage = re
            .captures(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .unwrap()
            .unwrap();
        info!("loss_percentage: {}", loss_percentage);
        assert!(loss_percentage == 0);
    }
    // info!("====================================================");
    // After set the IIDLossDevice, the average loss rate should be between 40%-60%
    {
        let _span = span!(Level::INFO, "ping_with_loss").entered();
        info!("try to ping with loss set to 0.5");
        left_control_interface
            .set_config(IIDLossDeviceConfig::new(0.5))
            .unwrap();
        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        let handle = std::process::Command::new("ping")
            .args(["192.168.12.1", "-c", "50", "-i", "0.3"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"(\d+)% packet loss").unwrap();
        let loss_percentage = re
            .captures(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .unwrap()
            .unwrap();
        info!("loss_percentage: {}", loss_percentage);
        assert!(loss_percentage >= 40 && loss_percentage <= 60);
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}
