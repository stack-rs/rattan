use clap::{Parser, ValueEnum};
use paste::paste;
use rand::rngs::StdRng;
use rand::SeedableRng;
use rattan::core::{RattanMachine, RattanMachineConfig};
use rattan::devices::bandwidth::queue::{
    CoDelQueue, CoDelQueueConfig, DropHeadQueue, DropHeadQueueConfig, DropTailQueue,
    DropTailQueueConfig,
};
use rattan::devices::bandwidth::{queue::InfiniteQueue, BwDevice};
use rattan::devices::delay::{DelayDevice, DelayDeviceConfig};
use rattan::devices::external::VirtualEthernet;
use rattan::devices::loss::{LossDevice, LossDeviceConfig};
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

// mod docker;

#[derive(Debug, Parser, Clone)]
pub struct CommandArgs {
    /// Verbose debug output
    // #[arg(short, long)]
    // verbose: bool,

    /// Run in docker mode
    #[arg(long)]
    docker: bool,

    /// Uplink packet loss
    #[arg(long, value_name = "Loss")]
    uplink_loss: Option<f64>,
    /// Downlink packet loss
    #[arg(long, value_name = "Loss")]
    downlink_loss: Option<f64>,

    /// Uplink delay
    #[arg(long, value_name = "Delay", value_parser = humantime::parse_duration)]
    uplink_delay: Option<Delay>,
    /// Downlink delay
    #[arg(long, value_name = "Delay", value_parser = humantime::parse_duration)]
    downlink_delay: Option<Delay>,

    /// Uplink bandwidth
    #[arg(long, value_name = "Bandwidth", group = "uplink-bw")]
    uplink_bandwidth: Option<u64>,
    /// Uplink trace file
    #[arg(long, value_name = "Trace File", group = "uplink-bw")]
    uplink_trace: Option<String>,
    /// Uplink queue type
    #[arg(
        long,
        value_name = "Queue Type",
        group = "uplink-queue",
        requires = "uplink-bw"
    )]
    uplink_queue: Option<QueueType>,
    /// Uplink queue arguments
    #[arg(long, value_name = "JSON", requires = "uplink-queue")]
    uplink_queue_args: Option<String>,

    /// Downlink bandwidth
    #[arg(long, value_name = "Bandwidth", group = "downlink-bw")]
    downlink_bandwidth: Option<u64>,
    /// Downlink trace file
    #[arg(long, value_name = "Trace File", group = "downlink-bw")]
    downlink_trace: Option<String>,
    /// Downlink queue type
    #[arg(
        long,
        value_name = "Queue Type",
        group = "downlink-queue",
        requires = "downlink-bw"
    )]
    downlink_queue: Option<QueueType>,
    /// Downlink queue arguments
    #[arg(long, value_name = "JSON", requires = "downlink-queue")]
    downlink_queue_args: Option<String>,

    /// Commands to run
    commands: Vec<String>,
}

#[derive(ValueEnum, Clone, Debug)]
#[value(rename_all = "lower")]
enum QueueType {
    Infinite,
    DropTail,
    DropHead,
    CoDel,
}

// Deserialize queue args, create queue, create BwDevice and add to machine
// $type: queue type (key in `QueueType`)
// $args: queue args
// $machine: RattanMachine
// $bandwidth: bandwidth for BwDevice
macro_rules! q_args_into_machine {
    ($type:ident, $args:expr, $machine:expr, $bandwidth:expr) => {
        paste!(
            match serde_json::from_str::<[<$type QueueConfig>]> (&$args.unwrap_or("{}".to_string())) {
                Ok(config) => {
                    let packet_queue: [<$type Queue>]<StdPacket> = config.into();
                    let bw_device = BwDevice::new($bandwidth, packet_queue);
                    $machine.add_device(bw_device)
                }
                Err(e) => {
                    error!("Failed to parse uplink-queue-args: {}", e);
                    return;
                }
            }
        )
    };
}

fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    let opts = CommandArgs::parse();
    // if opts.docker {
    //     docker::docker_main(opts).unwrap();
    //     return;
    // }
    info!("{:?}", opts);

    let _std_env = get_std_env(StdNetEnvConfig {
        mode: rattan::env::StdNetEnvMode::Compatible,
    })
    .unwrap();
    let left_ns = _std_env.left_ns.clone();
    let _right_ns = _std_env.right_ns.clone();

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    let rattan_opts = opts.clone();
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
                info!(left_device_rx, left_device_tx, "Left device");
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx, "Right device");
                let mut left_fd = vec![left_device_rx];
                let mut right_fd = vec![right_device_rx];

                if let Some(bandwidth) = rattan_opts.uplink_bandwidth {
                    let bandwidth = Bandwidth::from_bps(bandwidth);
                    let (bw_rx, bw_tx) = match rattan_opts.uplink_queue {
                        Some(QueueType::Infinite) | None => {
                            let packet_queue = InfiniteQueue::new();
                            let bw_device = BwDevice::new(bandwidth, packet_queue);
                            machine.add_device(bw_device)
                        }
                        Some(QueueType::DropTail) => {
                            q_args_into_machine!(
                                DropTail,
                                rattan_opts.uplink_queue_args,
                                machine,
                                bandwidth
                            )
                        }
                        Some(QueueType::DropHead) => {
                            q_args_into_machine!(
                                DropHead,
                                rattan_opts.uplink_queue_args,
                                machine,
                                bandwidth
                            )
                        }
                        Some(QueueType::CoDel) => {
                            q_args_into_machine!(
                                CoDel,
                                rattan_opts.uplink_queue_args,
                                machine,
                                bandwidth
                            )
                        }
                    };
                    info!(bw_rx, bw_tx, "Uplink bandwidth");
                    left_fd.push(bw_tx);
                    left_fd.push(bw_rx);
                }

                if let Some(bandwidth) = rattan_opts.downlink_bandwidth {
                    let bandwidth = Bandwidth::from_bps(bandwidth);
                    let (bw_rx, bw_tx) = match rattan_opts.downlink_queue {
                        Some(QueueType::Infinite) | None => {
                            let packet_queue = InfiniteQueue::new();
                            let bw_device = BwDevice::new(bandwidth, packet_queue);
                            machine.add_device(bw_device)
                        }
                        Some(QueueType::DropTail) => {
                            q_args_into_machine!(
                                DropTail,
                                rattan_opts.downlink_queue_args,
                                machine,
                                bandwidth
                            )
                        }
                        Some(QueueType::DropHead) => {
                            q_args_into_machine!(
                                DropHead,
                                rattan_opts.downlink_queue_args,
                                machine,
                                bandwidth
                            )
                        }
                        Some(QueueType::CoDel) => {
                            q_args_into_machine!(
                                CoDel,
                                rattan_opts.downlink_queue_args,
                                machine,
                                bandwidth
                            )
                        }
                    };
                    info!(bw_rx, bw_tx, "Downlink bandwidth");
                    right_fd.push(bw_tx);
                    right_fd.push(bw_rx);
                }

                if let Some(delay) = rattan_opts.uplink_delay {
                    let delay_device = DelayDevice::<StdPacket>::new();
                    let delay_ctl = delay_device.control_interface();
                    delay_ctl.set_config(DelayDeviceConfig::new(delay)).unwrap();
                    let (delay_rx, delay_tx) = machine.add_device(delay_device);
                    info!(delay_rx, delay_tx, "Uplink delay");
                    left_fd.push(delay_tx);
                    left_fd.push(delay_rx);
                }

                if let Some(delay) = rattan_opts.downlink_delay {
                    let delay_device = DelayDevice::<StdPacket>::new();
                    let delay_ctl = delay_device.control_interface();
                    delay_ctl.set_config(DelayDeviceConfig::new(delay)).unwrap();
                    let (delay_rx, delay_tx) = machine.add_device(delay_device);
                    info!(delay_rx, delay_tx, "Downlink delay");
                    right_fd.push(delay_tx);
                    right_fd.push(delay_rx);
                }

                if let Some(loss) = rattan_opts.uplink_loss {
                    let loss_device = LossDevice::<StdPacket, StdRng>::new(rng.clone());
                    let loss_ctl = loss_device.control_interface();
                    loss_ctl.set_config(LossDeviceConfig::new([loss])).unwrap();
                    let (loss_rx, loss_tx) = machine.add_device(loss_device);
                    info!(loss_rx, loss_tx, "Uplink loss");
                    left_fd.push(loss_tx);
                    left_fd.push(loss_rx);
                }

                if let Some(loss) = rattan_opts.downlink_loss {
                    let loss_device = LossDevice::<StdPacket, StdRng>::new(rng);
                    let loss_ctl = loss_device.control_interface();
                    loss_ctl.set_config(LossDeviceConfig::new([loss])).unwrap();
                    let (loss_rx, loss_tx) = machine.add_device(loss_device);
                    info!(loss_rx, loss_tx, "Downlink loss");
                    right_fd.push(loss_tx);
                    right_fd.push(loss_rx);
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
    let res = {
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
    };
    match res {
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
