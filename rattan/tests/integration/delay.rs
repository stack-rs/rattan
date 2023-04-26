/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test delay -- --nocapture
use rattan::core::{RattanMachine, RattanMachineConfig};
use rattan::devices::delay::{DelayDevice, DelayDeviceConfig};
use rattan::devices::external::VirtualEthernet;
use rattan::devices::{ControlInterface, Device, StdPacket};
use rattan::env::{get_std_env, StdNetEnvConfig};
use rattan::metal::io::AfPacketDriver;
use regex::Regex;
use std::time::Duration;
use tokio::sync::oneshot;

#[test]
fn test_delay() {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let left_ns = _std_env.left_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let (control_tx, control_rx) = oneshot::channel();

    let rattan_thread = std::thread::spawn(move || {
        let original_ns = _std_env.rattan_ns.enter().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async move {
            let left_delay_device = DelayDevice::<StdPacket>::new();
            let right_delay_device = DelayDevice::<StdPacket>::new();
            let left_control_interface = left_delay_device.control_interface();
            let right_control_interface = right_delay_device.control_interface();
            if let Err(_) = control_tx.send((left_control_interface, right_control_interface)) {
                eprintln!("send control interface failed");
            }
            let left_device =
                VirtualEthernet::<StdPacket, AfPacketDriver>::new(_std_env.left_pair.right.clone());
            let right_device =
                VirtualEthernet::<StdPacket, AfPacketDriver>::new(_std_env.right_pair.left.clone());

            let (left_delay_rx, left_delay_tx) = machine.add_device(left_delay_device);
            let (right_delay_rx, right_delay_tx) = machine.add_device(right_delay_device);
            let (left_device_rx, left_device_tx) = machine.add_device(left_device);
            let (right_device_rx, right_device_tx) = machine.add_device(right_device);

            machine.link_device(left_device_rx, left_delay_tx);
            machine.link_device(left_delay_rx, right_device_tx);
            machine.link_device(right_device_rx, right_delay_tx);
            machine.link_device(right_delay_rx, left_device_tx);

            let config = RattanMachineConfig {
                original_ns,
                port: 8082,
            };
            machine.core_loop(config).await
        });
    });

    let (left_control_interface, right_control_interface) = control_rx.blocking_recv().unwrap();

    // Before set the DelayDevice, the average latency should be less than 0.1ms
    {
        println!("try to ping with no delay");
        left_ns.enter().unwrap();
        let handle = std::process::Command::new("ping")
            .args(["192.168.12.1", "-c", "10"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"time=(\d+)").unwrap();
        let mut latency = re
            .captures_iter(&stdout)
            .map(|cap| cap[1].parse::<f64>())
            .flatten()
            .collect::<Vec<_>>();

        assert_eq!(latency.len(), 10);
        latency.drain(0..5);
        let average_latency = latency.iter().sum::<f64>() / latency.len() as f64;
        println!("average latency: {}", average_latency);
        assert!(average_latency < 10.0);
    }
    println!("====================================================");
    // After set the DelayDevice, the average latency should be around 200ms
    {
        println!("try to ping with delay set to 100ms (rtt = 200ms)");
        left_control_interface
            .set_config(DelayDeviceConfig::new(Duration::from_millis(100)))
            .unwrap();
        right_control_interface
            .set_config(DelayDeviceConfig::new(Duration::from_millis(100)))
            .unwrap();
        left_ns.enter().unwrap();
        let handle = std::process::Command::new("ping")
            .args(["192.168.12.1", "-c", "10"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"time=(\d+)").unwrap();
        let mut latency = re
            .captures_iter(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .flatten()
            .collect::<Vec<_>>();

        assert_eq!(latency.len(), 10);
        latency.drain(0..5);
        let average_latency = latency.iter().sum::<u64>() / latency.len() as u64;
        println!("average latency: {}", average_latency);
        assert!(average_latency > 195 && average_latency < 205);
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}
