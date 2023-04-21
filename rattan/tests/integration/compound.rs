/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test compound -- --nocapture
use netem_trace::Bandwidth;
use rand::rngs::StdRng;
use rand::SeedableRng;
use rattan::core::RattanMachine;
use rattan::devices::bandwidth::{BwDevice, BwDeviceConfig, BwDeviceControlInterface};
use rattan::devices::delay::{DelayDevice, DelayDeviceConfig, DelayDeviceControlInterface};
use rattan::devices::loss::{LossDevice, LossDeviceConfig, LossDeviceControlInterface};

use rattan::devices::external::VirtualEthernet;
use rattan::devices::{ControlInterface, Device, StdPacket};
use rattan::env::get_std_env;
use rattan::metal::io::AfPacketDriver;
use regex::Regex;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::oneshot;

#[test]
fn test_compound() {
    let _std_env = get_std_env().unwrap();
    let left_ns = _std_env.left_ns.clone();
    let right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let (delay_control_tx, delay_control_rx) = oneshot::channel();
    let (loss_control_tx, loss_control_rx) = oneshot::channel();
    let (bw_control_tx, bw_control_rx) = oneshot::channel();

    let rattan_thread = std::thread::spawn(move || {
        _std_env.rattan_ns.enter().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async move {
            let rng = StdRng::seed_from_u64(42);
            let left_bw_device = BwDevice::<StdPacket>::new();
            let right_bw_device = BwDevice::<StdPacket>::new();
            let left_delay_device = DelayDevice::<StdPacket>::new();
            let right_delay_device = DelayDevice::<StdPacket>::new();
            let left_loss_device = LossDevice::<StdPacket, StdRng>::new(rng.clone());
            let right_loss_device = LossDevice::<StdPacket, StdRng>::new(rng);

            let left_bw_ctl = left_bw_device.control_interface();
            let right_bw_ctl = right_bw_device.control_interface();
            if let Err(_) = bw_control_tx.send((left_bw_ctl, right_bw_ctl)) {
                eprintln!("send control interface failed");
            }

            let left_delay_ctl = left_delay_device.control_interface();
            let right_delay_ctl = right_delay_device.control_interface();
            if let Err(_) = delay_control_tx.send((left_delay_ctl, right_delay_ctl)) {
                eprintln!("send control interface failed");
            }

            let left_loss_ctl = left_loss_device.control_interface();
            let right_loss_ctl = right_loss_device.control_interface();
            if let Err(_) = loss_control_tx.send((left_loss_ctl, right_loss_ctl)) {
                eprintln!("send control interface failed");
            }

            let left_device =
                VirtualEthernet::<StdPacket, AfPacketDriver>::new(_std_env.left_pair.right.clone());
            let right_device =
                VirtualEthernet::<StdPacket, AfPacketDriver>::new(_std_env.right_pair.left.clone());

            let (left_bw_rx, left_bw_tx) = machine.add_device(left_bw_device);
            let (right_bw_rx, right_bw_tx) = machine.add_device(right_bw_device);
            let (left_delay_rx, left_delay_tx) = machine.add_device(left_delay_device);
            let (right_delay_rx, right_delay_tx) = machine.add_device(right_delay_device);
            let (left_loss_rx, left_loss_tx) = machine.add_device(left_loss_device);
            let (right_loss_rx, right_loss_tx) = machine.add_device(right_loss_device);
            let (left_device_rx, left_device_tx) = machine.add_device(left_device);
            let (right_device_rx, right_device_tx) = machine.add_device(right_device);

            machine.link_device(left_device_rx, left_bw_tx);
            machine.link_device(left_bw_rx, left_delay_tx);
            machine.link_device(left_delay_rx, left_loss_tx);
            machine.link_device(left_loss_rx, right_device_tx);

            machine.link_device(right_device_rx, right_bw_tx);
            machine.link_device(right_bw_rx, right_delay_tx);
            machine.link_device(right_delay_rx, right_loss_tx);
            machine.link_device(right_loss_rx, left_device_tx);

            machine.core_loop().await
        });
    });

    let (mut left_bw_ctl, mut right_bw_ctl) = bw_control_rx.blocking_recv().unwrap();
    let (mut left_delay_ctl, mut right_delay_ctl) = delay_control_rx.blocking_recv().unwrap();
    let (mut left_loss_ctl, _) = loss_control_rx.blocking_recv().unwrap();

    // Before set the BwDevice, the bandwidth should be around 1Gbps
    {
        println!("try to iperf with no bandwidth limit");
        let handle = {
            right_ns.enter().unwrap();
            std::thread::spawn(|| {
                std::process::Command::new("iperf3")
                    .args(["-s", "-p", "9000", "-1"])
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
            })
        };
        left_ns.enter().unwrap();
        sleep(Duration::from_secs(1));
        let client_handle = std::process::Command::new("iperf3")
            .args([
                "-c",
                "192.168.2.1",
                "-p",
                "9000",
                "--cport",
                "10000",
                "-t",
                "10",
                "-J",
                "-C",
                "reno",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = client_handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        handle.join().unwrap();
        // println!("{}", stdout);
        let re = Regex::new(r#""bits_per_second":\s*(\d+)"#).unwrap();
        let bandwidth = re
            .captures_iter(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .flatten()
            .step_by(2)
            .take(10)
            .collect::<Vec<_>>();

        assert!(bandwidth.len() > 0);
    }
    println!("====================================================");
    // After set the BwDevice, the bandwidth should be between 90-100Mbps
    std::thread::sleep(std::time::Duration::from_secs(1));
    {
        println!("try to iperf with bandwidth limit set to 100Mbps");
        Arc::<BwDeviceControlInterface>::get_mut(&mut left_bw_ctl)
            .unwrap()
            .set_config(BwDeviceConfig::new(Bandwidth::from_mbps(100)))
            .unwrap();
        Arc::<BwDeviceControlInterface>::get_mut(&mut right_bw_ctl)
            .unwrap()
            .set_config(BwDeviceConfig::new(Bandwidth::from_mbps(100)))
            .unwrap();
        Arc::<DelayDeviceControlInterface>::get_mut(&mut left_delay_ctl)
            .as_mut()
            .unwrap()
            .set_config(DelayDeviceConfig::new(Duration::from_millis(50)))
            .unwrap();
        Arc::<DelayDeviceControlInterface>::get_mut(&mut right_delay_ctl)
            .as_mut()
            .unwrap()
            .set_config(DelayDeviceConfig::new(Duration::from_millis(50)))
            .unwrap();
        Arc::<LossDeviceControlInterface>::get_mut(&mut left_loss_ctl)
            .as_mut()
            .unwrap()
            .set_config(LossDeviceConfig::new(vec![0.01; 10]))
            .unwrap();

        let handle = {
            right_ns.enter().unwrap();
            std::thread::spawn(|| {
                std::process::Command::new("iperf3")
                    .args(["-s", "-p", "9001", "-1"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
            })
        };
        left_ns.enter().unwrap();
        sleep(Duration::from_secs(1));
        let client_handle = std::process::Command::new("iperf3")
            .args([
                "-c",
                "192.168.2.1",
                "-p",
                "9001",
                "--cport",
                "10001",
                "-t",
                "10",
                "-J",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = client_handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        println!("{}", stderr);
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
        println!("bitrate: {}", bitrate);
        // assert!(bitrate > 90000000 && bitrate < 100000000);
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}
