use std::process::Stdio;

use clap::{Parser, ValueEnum};
use paste::paste;
use rattan::config::{
    BwDeviceBuildConfig, BwReplayCLIConfig, BwReplayDeviceBuildConfig, DelayDeviceBuildConfig,
    DeviceBuildConfig, LossDeviceBuildConfig, RattanConfig, RattanCoreConfig,
};
use rattan::devices::bandwidth::queue::{
    CoDelQueueConfig, DropHeadQueueConfig, DropTailQueueConfig, InfiniteQueueConfig,
};
use rattan::devices::bandwidth::BwDeviceConfig;
use rattan::devices::StdPacket;
use rattan::env::{IODriver, StdNetEnvConfig, StdNetEnvMode};
use rattan::netem_trace::{Bandwidth, Delay};
use rattan::radix::RattanRadix;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[cfg(feature = "http")]
use rattan::control::http::HttpConfig;

// mod docker;

// const CONFIG_PORT_BASE: u16 = 8086;

#[derive(Debug, Parser, Clone)]
pub struct CommandArgs {
    // Verbose debug output
    // #[arg(short, long)]
    // verbose: bool,
    // Run in docker mode
    // #[arg(long)]
    // docker: bool,
    /// Use config file and ignore other options
    #[arg(short, long, value_name = "Config File")]
    config: Option<String>,

    /// Only generate config and exit
    #[arg(long)]
    gen_config: bool,

    #[cfg(feature = "http")]
    /// Enable HTTP control server
    #[arg(long)]
    http: bool,
    #[cfg(feature = "http")]
    /// HTTP control server port (default: 8086)
    #[arg(short, long, value_name = "Port")]
    port: Option<u16>,

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

    /// Command to run
    command: Vec<String>,
}

#[derive(ValueEnum, Clone, Debug)]
#[value(rename_all = "lower")]
enum QueueType {
    Infinite,
    DropTail,
    DropHead,
    CoDel,
}

// Deserialize queue args and create BwDeviceBuildConfig
// $q_type: queue type (key in `QueueType`)
// $q_args: queue args
// $bw: bandwidth for BwDevice
macro_rules! bw_q_args_into_config {
    ($q_type:ident, $q_args:expr, $bw:expr) => {
        paste!(
            match serde_json::from_str::<[<$q_type QueueConfig>]> (&$q_args.unwrap_or("{}".to_string())) {
                Ok(queue_config) => DeviceBuildConfig::Bw(BwDeviceBuildConfig::$q_type(
                    if $q_args.is_none() {
                        BwDeviceConfig::new($bw, None, None)
                    } else {
                        BwDeviceConfig::new($bw, queue_config, None)
                    }
                )),
                Err(e) => {
                    error!("Failed to parse queue args {:?}: {}", $q_args, e);
                    return Err(anyhow::anyhow!("Failed to parse queue args {:?}: {}", $q_args, e));
                }
            }
        )
    };
}

// Deserialize queue args and create BwReplayDeviceBuildConfig
// $q_type: queue type (key in `QueueType`)
// $q_args: queue args
// $mahimahi_trace: mahimahi_trace for BwReplayDevice
macro_rules! bwreplay_q_args_into_config {
    ($q_type:ident, $q_args:expr, $mahimahi_trace:expr) => {
        paste!(
            match serde_json::from_str::<[<$q_type QueueConfig>]> (&$q_args.unwrap_or("{}".to_string())) {
                Ok(queue_config) => DeviceBuildConfig::BwReplay(BwReplayDeviceBuildConfig::$q_type(
                    if $q_args.is_none() {
                        BwReplayCLIConfig::new($mahimahi_trace, None, None, None)
                    } else {
                        BwReplayCLIConfig::new($mahimahi_trace, None, queue_config, None)
                    }
                )),
                Err(e) => {
                    error!("Failed to parse queue args {:?}: {}", $q_args, e);
                    return Err(anyhow::anyhow!("Failed to parse queue args {:?}: {}", $q_args, e));
                }
            }
        )
    };
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let opts = CommandArgs::parse();
    debug!("{:?}", opts);
    // if opts.docker {
    //     docker::docker_main(opts).unwrap();
    //     return;
    // }

    let config = match opts.config {
        Some(config_file) => {
            info!("Loading config from {}", config_file);
            let content = config::Config::builder()
                .add_source(config::File::with_name(&config_file))
                .build()?;
            let config: RattanConfig<StdPacket> = content.try_deserialize()?;
            config
        }
        None => {
            #[cfg(feature = "http")]
            let mut http_config = HttpConfig {
                enable: opts.http,
                ..Default::default()
            };
            #[cfg(feature = "http")]
            if let Some(port) = opts.port {
                http_config.port = port;
            }

            let mut core_config = RattanCoreConfig::<StdPacket>::default();
            let mut uplink_count = 0;
            let mut downlink_count = 0;

            if let Some(bandwidth) = opts.uplink_bandwidth {
                let bandwidth = Bandwidth::from_bps(bandwidth);
                let device_config = match opts.uplink_queue {
                    Some(QueueType::Infinite) | None => {
                        DeviceBuildConfig::Bw(BwDeviceBuildConfig::Infinite(BwDeviceConfig::new(
                            bandwidth,
                            InfiniteQueueConfig::new(),
                            None,
                        )))
                    }
                    Some(QueueType::DropTail) => {
                        bw_q_args_into_config!(DropTail, opts.uplink_queue_args.clone(), bandwidth)
                    }
                    Some(QueueType::DropHead) => {
                        bw_q_args_into_config!(DropHead, opts.uplink_queue_args.clone(), bandwidth)
                    }
                    Some(QueueType::CoDel) => {
                        bw_q_args_into_config!(CoDel, opts.uplink_queue_args.clone(), bandwidth)
                    }
                };
                uplink_count += 1;
                core_config
                    .devices
                    .insert(format!("up_{}", uplink_count), device_config);
            } else if let Some(trace_file) = opts.uplink_trace {
                let device_config = match opts.uplink_queue {
                    Some(QueueType::Infinite) | None => DeviceBuildConfig::BwReplay(
                        BwReplayDeviceBuildConfig::Infinite(BwReplayCLIConfig::new(
                            trace_file,
                            None,
                            InfiniteQueueConfig::new(),
                            None,
                        )),
                    ),
                    Some(QueueType::DropTail) => {
                        bwreplay_q_args_into_config!(
                            DropTail,
                            opts.uplink_queue_args.clone(),
                            trace_file
                        )
                    }
                    Some(QueueType::DropHead) => {
                        bwreplay_q_args_into_config!(
                            DropHead,
                            opts.uplink_queue_args.clone(),
                            trace_file
                        )
                    }
                    Some(QueueType::CoDel) => {
                        bwreplay_q_args_into_config!(
                            CoDel,
                            opts.uplink_queue_args.clone(),
                            trace_file
                        )
                    }
                };
                uplink_count += 1;
                core_config
                    .devices
                    .insert(format!("up_{}", uplink_count), device_config);
            }

            if let Some(bandwidth) = opts.downlink_bandwidth {
                let bandwidth = Bandwidth::from_bps(bandwidth);
                let device_config = match opts.downlink_queue {
                    Some(QueueType::Infinite) | None => {
                        DeviceBuildConfig::Bw(BwDeviceBuildConfig::Infinite(BwDeviceConfig::new(
                            bandwidth,
                            InfiniteQueueConfig::new(),
                            None,
                        )))
                    }
                    Some(QueueType::DropTail) => {
                        bw_q_args_into_config!(
                            DropTail,
                            opts.downlink_queue_args.clone(),
                            bandwidth
                        )
                    }
                    Some(QueueType::DropHead) => {
                        bw_q_args_into_config!(
                            DropHead,
                            opts.downlink_queue_args.clone(),
                            bandwidth
                        )
                    }
                    Some(QueueType::CoDel) => {
                        bw_q_args_into_config!(CoDel, opts.downlink_queue_args.clone(), bandwidth)
                    }
                };
                downlink_count += 1;
                core_config
                    .devices
                    .insert(format!("down_{}", downlink_count), device_config);
            } else if let Some(trace_file) = opts.downlink_trace {
                let device_config = match opts.downlink_queue {
                    Some(QueueType::Infinite) | None => DeviceBuildConfig::BwReplay(
                        BwReplayDeviceBuildConfig::Infinite(BwReplayCLIConfig::new(
                            trace_file,
                            None,
                            InfiniteQueueConfig::new(),
                            None,
                        )),
                    ),
                    Some(QueueType::DropTail) => {
                        bwreplay_q_args_into_config!(
                            DropTail,
                            opts.downlink_queue_args.clone(),
                            trace_file
                        )
                    }
                    Some(QueueType::DropHead) => {
                        bwreplay_q_args_into_config!(
                            DropHead,
                            opts.downlink_queue_args.clone(),
                            trace_file
                        )
                    }
                    Some(QueueType::CoDel) => {
                        bwreplay_q_args_into_config!(
                            CoDel,
                            opts.downlink_queue_args.clone(),
                            trace_file
                        )
                    }
                };
                downlink_count += 1;
                core_config
                    .devices
                    .insert(format!("down_{}", downlink_count), device_config);
            }

            if let Some(delay) = opts.uplink_delay {
                let device_config = DeviceBuildConfig::Delay(DelayDeviceBuildConfig::new(delay));
                uplink_count += 1;
                core_config
                    .devices
                    .insert(format!("up_{}", uplink_count), device_config);
            }

            if let Some(delay) = opts.downlink_delay {
                let device_config = DeviceBuildConfig::Delay(DelayDeviceBuildConfig::new(delay));
                downlink_count += 1;
                core_config
                    .devices
                    .insert(format!("down_{}", downlink_count), device_config);
            }

            if let Some(loss) = opts.uplink_loss {
                let device_config = DeviceBuildConfig::Loss(LossDeviceBuildConfig::new([loss]));
                uplink_count += 1;
                core_config
                    .devices
                    .insert(format!("up_{}", uplink_count), device_config);
            }

            if let Some(loss) = opts.downlink_loss {
                let device_config = DeviceBuildConfig::Loss(LossDeviceBuildConfig::new([loss]));
                downlink_count += 1;
                core_config
                    .devices
                    .insert(format!("down_{}", downlink_count), device_config);
            }

            for i in 1..uplink_count {
                core_config
                    .links
                    .insert(format!("up_{}", i), format!("up_{}", i + 1));
            }
            for i in 1..downlink_count {
                core_config
                    .links
                    .insert(format!("down_{}", i), format!("down_{}", i + 1));
            }

            if uplink_count > 0 {
                core_config
                    .links
                    .insert("left".to_string(), "up_1".to_string());
                core_config
                    .links
                    .insert(format!("up_{}", uplink_count), "right".to_string());
            } else {
                core_config
                    .links
                    .insert("left".to_string(), "right".to_string());
            }
            if downlink_count > 0 {
                core_config
                    .links
                    .insert("right".to_string(), "down_1".to_string());
                core_config
                    .links
                    .insert(format!("down_{}", downlink_count), "left".to_string());
            } else {
                core_config
                    .links
                    .insert("right".to_string(), "left".to_string());
            }

            RattanConfig::<StdPacket> {
                env: StdNetEnvConfig {
                    mode: StdNetEnvMode::Compatible,
                    driver: IODriver::Packet,
                },
                #[cfg(feature = "http")]
                http: http_config,
                core: core_config,
            }
        }
    };
    debug!(?config);
    if config.core.devices.is_empty() {
        warn!("No devices specified in config");
    }
    if config.core.links.is_empty() {
        warn!("No links specified in config");
    }

    if opts.gen_config {
        let toml_string = toml::to_string_pretty(&config)?;
        println!("{}", toml_string);
        return Ok(());
    }

    let mut radix = RattanRadix::<StdPacket>::new(config)?;
    radix.spawn_rattan()?;

    let rattan_base = radix.right_ip();
    let left_handle = radix.left_spawn(move || {
        let mut client_handle = std::process::Command::new("/usr/bin/env");
        client_handle.env("RATTAN_BASE", rattan_base.to_string());
        if opts.command.is_empty() {
            let shell = std::env::var("SHELL").unwrap_or("/bin/bash".to_string());
            client_handle.arg(shell);
        } else {
            client_handle.args(opts.command);
        }
        info!("Running {:?}", client_handle);
        let mut client_handle = client_handle
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        let output = client_handle.wait()?;
        match output.code() {
            Some(0) => info!("Exited with status code: 0"),
            Some(code) => warn!("Exited with status code: {}", code),
            None => error!("Process terminated by signal"),
        }
        anyhow::Result::Ok(())
    })?;
    radix.start_rattan()?;
    left_handle
        .join()
        .map_err(|e| anyhow::anyhow!("Error joining left handle: {:?}", e))??;

    // get the last byte of rattan_base as the port number
    // let port = CONFIG_PORT_BASE - 1
    //     + match rattan_base {
    //         std::net::IpAddr::V4(ip) => ip.octets()[3],
    //         std::net::IpAddr::V6(ip) => ip.octets()[15],
    //     } as u16;
    // let config = RattanMachineConfig { original_ns, port };

    Ok(())
}
