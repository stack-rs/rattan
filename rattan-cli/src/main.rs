use clap::Parser;
use rand::rngs::StdRng;
use rand::SeedableRng;
use rattan::core::{RattanMachine, RattanMachineConfig};
use rattan::devices::bandwidth::{BwDevice, BwDeviceConfig};
use rattan::devices::delay::{DelayDevice, DelayDeviceConfig};
use rattan::devices::external::VirtualEthernet;
use rattan::devices::loss::{IIDLossDevice, IIDLossDeviceConfig};
use rattan::devices::{ControlInterface, Device, StdPacket};
use rattan::env::{get_std_env, StdNetEnvConfig};
use rattan::metal::io::AfPacketDriver;
use rattan::metal::netns::NetNsGuard;
use rattan::netem_trace::{Bandwidth, Delay};
use std::process::Stdio;
use std::thread::sleep;
use std::time::Duration;
use tracing::{error, info, span, Instrument, Level};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
struct CommandArgs {
    /// Verbose debug output
    #[arg(short, long)]
    verbose: bool,
    #[arg(long, short)]
    loss: Option<f64>,
    #[arg(long, short, value_parser = humantime::parse_duration)]
    delay: Option<Delay>,
    #[arg(long, short)]
    bandwidth: Option<u64>,
    commands: Vec<String>,
}

fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    let opts = CommandArgs::parse();
    let loss = opts.loss;
    let delay = opts.delay;
    let bandwidth = opts.bandwidth.map(Bandwidth::from_bps);
    info!("{:?}", opts);

    let _std_env = get_std_env(StdNetEnvConfig {
        mode: rattan::env::StdNetEnvMode::Compatible,
    })
    .unwrap();
    let left_ns = _std_env.left_ns.clone();
    let _right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

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

                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.left_pair.right.clone(),
                );
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(
                    _std_env.right_pair.left.clone(),
                );

                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);
                let mut left_fd = vec![left_device_rx];
                let mut right_fd = vec![right_device_rx];
                if let Some(bandwidth) = bandwidth {
                    let left_bw_device = BwDevice::<StdPacket>::new();
                    let right_bw_device = BwDevice::<StdPacket>::new();
                    let left_bw_ctl = left_bw_device.control_interface();
                    let right_bw_ctl = right_bw_device.control_interface();
                    left_bw_ctl
                        .set_config(BwDeviceConfig::new(bandwidth))
                        .unwrap();
                    right_bw_ctl
                        .set_config(BwDeviceConfig::new(bandwidth))
                        .unwrap();
                    let (left_bw_rx, left_bw_tx) = machine.add_device(left_bw_device);
                    info!(left_bw_rx, left_bw_tx);
                    let (right_bw_rx, right_bw_tx) = machine.add_device(right_bw_device);
                    info!(right_bw_rx, right_bw_tx);
                    left_fd.push(left_bw_tx);
                    left_fd.push(left_bw_rx);
                    right_fd.push(right_bw_tx);
                    right_fd.push(right_bw_rx);
                }
                if let Some(delay) = delay {
                    let left_delay_device = DelayDevice::<StdPacket>::new();
                    let right_delay_device = DelayDevice::<StdPacket>::new();
                    let left_delay_ctl = left_delay_device.control_interface();
                    let right_delay_ctl = right_delay_device.control_interface();
                    left_delay_ctl
                        .set_config(DelayDeviceConfig::new(delay))
                        .unwrap();
                    right_delay_ctl
                        .set_config(DelayDeviceConfig::new(delay))
                        .unwrap();
                    let (left_delay_rx, left_delay_tx) = machine.add_device(left_delay_device);
                    info!(left_delay_rx, left_delay_tx);
                    let (right_delay_rx, right_delay_tx) = machine.add_device(right_delay_device);
                    info!(right_delay_rx, right_delay_tx);
                    left_fd.push(left_delay_tx);
                    left_fd.push(left_delay_rx);
                    right_fd.push(right_delay_tx);
                    right_fd.push(right_delay_rx);
                }
                if let Some(loss) = loss {
                    let left_loss_device = IIDLossDevice::<StdPacket, StdRng>::new(rng.clone());
                    let right_loss_device = IIDLossDevice::<StdPacket, StdRng>::new(rng);
                    let left_loss_ctl = left_loss_device.control_interface();
                    left_loss_ctl
                        .set_config(IIDLossDeviceConfig::new(loss))
                        .unwrap();
                    let (left_loss_rx, left_loss_tx) = machine.add_device(left_loss_device);
                    info!(left_loss_rx, left_loss_tx);
                    let (right_loss_rx, right_loss_tx) = machine.add_device(right_loss_device);
                    info!(right_loss_rx, right_loss_tx);
                    left_fd.push(left_loss_tx);
                    left_fd.push(left_loss_rx);
                    right_fd.push(right_loss_tx);
                    right_fd.push(right_loss_rx);
                }

                left_fd.push(right_device_tx);
                if left_fd.len() % 2 != 0 {
                    panic!("Wrong number of devices");
                }
                for i in 0..left_fd.len() / 2 {
                    machine.link_device(left_fd[i * 2], left_fd[i * 2 + 1]);
                }
                right_fd.push(left_device_tx);
                if right_fd.len() % 2 != 0 {
                    panic!("Wrong number of devices");
                }
                for i in 0..right_fd.len() / 2 {
                    machine.link_device(right_fd[i * 2], right_fd[i * 2 + 1]);
                }

                let config = RattanMachineConfig {
                    original_ns,
                    port: 8086,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    // Test connectivity before starting
    match {
        let _span = span!(Level::INFO, "ping_test").entered();
        info!("ping testing...");
        let _left_ns_guard = NetNsGuard::new(left_ns.clone()).unwrap();
        let handle = std::process::Command::new("ping")
            .args(["192.168.12.1", "-c", "5", "-i", "0.2"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = handle.wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        stdout.contains("time=")
    } {
        true => {
            info!("ping test passed");
            left_ns.enter().unwrap();
            sleep(Duration::from_secs(1));
            let mut client_handle = std::process::Command::new("/bin/bash");
            if !opts.commands.is_empty() {
                client_handle.arg("-c").args(opts.commands);
            }
            let mut client_handle = client_handle
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let output = client_handle.wait().unwrap();
            info!("Exit {}", output.code().unwrap());
        }
        false => {
            error!("ping test failed");
        }
    };

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}
