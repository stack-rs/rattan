use std::{collections::HashMap, process::Stdio};

use clap::{command, Args, Parser, Subcommand, ValueEnum};
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use paste::paste;
use rattan::config::{
    BwDeviceBuildConfig, BwReplayDeviceBuildConfig, BwReplayQueueConfig, DelayDeviceBuildConfig,
    DeviceBuildConfig, LossDeviceBuildConfig, RattanConfig,
};
use rattan::devices::bandwidth::queue::{
    CoDelQueueConfig, DropHeadQueueConfig, DropTailQueueConfig, InfiniteQueueConfig,
};
use rattan::devices::bandwidth::BwDeviceConfig;
use rattan::devices::StdPacket;
use rattan::env::{StdNetEnvConfig, StdNetEnvMode};
use rattan::metal::io::af_packet::AfPacketDriver;
use rattan::netem_trace::{Bandwidth, Delay};
use rattan::radix::RattanRadix;
use tracing::{debug, error, info, warn};
use tracing_subscriber::filter::{self, FilterFn};
use tracing_subscriber::Layer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[cfg(feature = "http")]
use rattan::control::http::HttpConfig;

// mod docker;

// const CONFIG_PORT_BASE: u16 = 8086;

#[derive(Debug, Parser, Clone)]
#[command(rename_all = "kebab-case")]
#[command(propagate_version = true)]
pub struct Arguments {
    // Verbose debug output
    // #[arg(short, long)]
    // verbose: bool,
    // Run in docker mode
    // #[arg(long)]
    // docker: bool,
    /// Use config file and ignore other options
    #[arg(short, long, value_name = "Config File")]
    config: Option<String>,

    #[command(subcommand)]
    subcommand: Option<CliCommand>,

    #[cfg(feature = "http")]
    /// Enable HTTP control server
    #[arg(long)]
    http: bool,
    #[cfg(feature = "http")]
    /// HTTP control server port (default: 8086)
    #[arg(short, long, value_name = "Port")]
    port: Option<u16>,

    /// Uplink packet loss
    #[arg(long, global = true, value_name = "Loss")]
    uplink_loss: Option<f64>,
    /// Downlink packet loss
    #[arg(long, global = true, value_name = "Loss")]
    downlink_loss: Option<f64>,

    /// Uplink delay
    #[arg(long, global = true, value_name = "Delay", value_parser = humantime::parse_duration)]
    uplink_delay: Option<Delay>,
    /// Downlink delay
    #[arg(long, global = true, value_name = "Delay", value_parser = humantime::parse_duration)]
    downlink_delay: Option<Delay>,

    /// Uplink bandwidth
    #[arg(long, global = true, value_name = "Bandwidth", group = "uplink-bw")]
    uplink_bandwidth: Option<u64>,
    /// Uplink trace file
    #[arg(long, global = true, value_name = "Trace File", group = "uplink-bw")]
    uplink_trace: Option<String>,
    /// Uplink queue type
    #[arg(
        long,
        global = true,
        value_name = "Queue Type",
        group = "uplink-queue",
        requires = "uplink-bw"
    )]
    uplink_queue: Option<QueueType>,
    /// Uplink queue arguments
    #[arg(long, global = true, value_name = "JSON", requires = "uplink-queue")]
    uplink_queue_args: Option<String>,

    /// Downlink bandwidth
    #[arg(long, global = true, value_name = "Bandwidth", group = "downlink-bw")]
    downlink_bandwidth: Option<u64>,
    /// Downlink trace file
    #[arg(long, global = true, value_name = "Trace File", group = "downlink-bw")]
    downlink_trace: Option<String>,
    /// Downlink queue type
    #[arg(
        long,
        global = true,
        value_name = "Queue Type",
        group = "downlink-queue",
        requires = "downlink-bw"
    )]
    downlink_queue: Option<QueueType>,
    /// Downlink queue arguments
    #[arg(long, global = true, value_name = "JSON", requires = "downlink-queue")]
    downlink_queue_args: Option<String>,

    /// Enable packet logging
    #[arg(long)]
    packet_log: bool,
    /// Packet log path, default to $CACHE_DIR/rattan/packet.log
    #[arg(long, value_name = "Log File", requires = "packet-log")]
    packet_log_path: Option<String>,

    /// Command to run
    #[arg(last = true)]
    command: Vec<String>,
}

#[derive(Subcommand, Debug, Clone)]
enum CliCommand {
    /// Generate the config.
    Generate(GenerateArgs),
}

#[derive(Args, Debug, Default, Clone)]
#[command(rename_all = "kebab-case")]
pub struct GenerateArgs {
    /// The output file path of the config. Default to stdout.
    #[arg(short, long)]
    pub output: Option<String>,
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
    ($q_type:ident, $q_args:expr, $trace_file:expr) => {
        paste!(
            match serde_json::from_str::<[<$q_type QueueConfig>]> (&$q_args.unwrap_or("{}".to_string())) {
                Ok(queue_config) => DeviceBuildConfig::BwReplay(BwReplayDeviceBuildConfig::$q_type(
                    if $q_args.is_none() {
                        BwReplayQueueConfig::new($trace_file, None, None)
                    } else {
                        BwReplayQueueConfig::new($trace_file, queue_config, None)
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
    let opts = Arguments::parse();
    debug!("{:?}", opts);
    // if opts.docker {
    //     docker::docker_main(opts).unwrap();
    //     return;
    // }
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "warn".into()),
            )
            .with_filter(filter::filter_fn(|metadata| {
                !metadata.target().ends_with("packet")
            })),
    );
    let _guard: Option<_> = if opts.packet_log {
        if let Some((log_dir, file_name)) = opts
            .packet_log_path
            .and_then(|path| {
                let path = std::path::PathBuf::from(path);
                let file_name = path.file_name().and_then(|f| f.to_str());
                let log_dir = path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .and_then(|p| file_name.map(|f| (p, f.to_string())));
                log_dir
            })
            .or_else(|| {
                dirs::cache_dir()
                    .map(|mut p| {
                        p.push("rattan");
                        p
                    })
                    .map(|log_dir| (log_dir, "packet.log".to_string()))
            })
        {
            std::fs::create_dir_all(&log_dir)?;
            let file_logger = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join(file_name))?;
            // let file_logger = tracing_appender::rolling::daily(log_dir, file_name_prefix);
            let (non_blocking, guard) = tracing_appender::non_blocking(file_logger);
            let file_log_filter = FilterFn::new(|metadata| {
                // Only enable spans or events with the target "interesting_things"
                metadata.target().ends_with("packet")
            });
            let env_filter = tracing_subscriber::EnvFilter::try_from_env("RATTAN_PACKET_LOG")
                .unwrap_or_else(|_| "info".into());
            subscriber
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .with_filter(file_log_filter)
                        .with_filter(env_filter),
                )
                .init();
            Some(guard)
        } else {
            subscriber.init();
            None
        }
    } else {
        subscriber.init();
        None
    };

    let config = match opts.config {
        Some(config_file) => {
            info!("Loading config from {}", config_file);
            let config: RattanConfig<StdPacket> = Figment::new()
                .merge(Toml::file(&config_file))
                .merge(Env::prefixed("RATTAN_"))
                .extract()?;

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

            let mut devices_config = HashMap::<String, DeviceBuildConfig<StdPacket>>::new();
            let mut links_config = HashMap::<String, String>::new();
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
                devices_config.insert(format!("up_{}", uplink_count), device_config);
            } else if let Some(trace_file) = opts.uplink_trace {
                let device_config = match opts.uplink_queue {
                    Some(QueueType::Infinite) | None => {
                        DeviceBuildConfig::BwReplay(BwReplayDeviceBuildConfig::Infinite(
                            BwReplayQueueConfig::new(trace_file, InfiniteQueueConfig::new(), None),
                        ))
                    }
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
                devices_config.insert(format!("up_{}", uplink_count), device_config);
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
                devices_config.insert(format!("down_{}", downlink_count), device_config);
            } else if let Some(trace_file) = opts.downlink_trace {
                let device_config = match opts.downlink_queue {
                    Some(QueueType::Infinite) | None => {
                        DeviceBuildConfig::BwReplay(BwReplayDeviceBuildConfig::Infinite(
                            BwReplayQueueConfig::new(trace_file, InfiniteQueueConfig::new(), None),
                        ))
                    }
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
                devices_config.insert(format!("down_{}", downlink_count), device_config);
            }

            if let Some(delay) = opts.uplink_delay {
                let device_config = DeviceBuildConfig::Delay(DelayDeviceBuildConfig::new(delay));
                uplink_count += 1;
                devices_config.insert(format!("up_{}", uplink_count), device_config);
            }

            if let Some(delay) = opts.downlink_delay {
                let device_config = DeviceBuildConfig::Delay(DelayDeviceBuildConfig::new(delay));
                downlink_count += 1;
                devices_config.insert(format!("down_{}", downlink_count), device_config);
            }

            if let Some(loss) = opts.uplink_loss {
                let device_config = DeviceBuildConfig::Loss(LossDeviceBuildConfig::new([loss]));
                uplink_count += 1;
                devices_config.insert(format!("up_{}", uplink_count), device_config);
            }

            if let Some(loss) = opts.downlink_loss {
                let device_config = DeviceBuildConfig::Loss(LossDeviceBuildConfig::new([loss]));
                downlink_count += 1;
                devices_config.insert(format!("down_{}", downlink_count), device_config);
            }

            for i in 1..uplink_count {
                links_config.insert(format!("up_{}", i), format!("up_{}", i + 1));
            }
            for i in 1..downlink_count {
                links_config.insert(format!("down_{}", i), format!("down_{}", i + 1));
            }

            if uplink_count > 0 {
                links_config.insert("left".to_string(), "up_1".to_string());
                links_config.insert(format!("up_{}", uplink_count), "right".to_string());
            } else {
                links_config.insert("left".to_string(), "right".to_string());
            }
            if downlink_count > 0 {
                links_config.insert("right".to_string(), "down_1".to_string());
                links_config.insert(format!("down_{}", downlink_count), "left".to_string());
            } else {
                links_config.insert("right".to_string(), "left".to_string());
            }

            RattanConfig::<StdPacket> {
                env: StdNetEnvConfig {
                    mode: StdNetEnvMode::Compatible,
                    client_cores: vec![1],
                    server_cores: vec![3],
                },
                #[cfg(feature = "http")]
                http: http_config,
                devices: devices_config,
                links: links_config,
                resource: Default::default(),
            }
        }
    };
    debug!(?config);
    if config.devices.is_empty() {
        warn!("No devices specified in config");
    }
    if config.links.is_empty() {
        warn!("No links specified in config");
    }

    if let Some(cmd) = opts.subcommand {
        match cmd {
            CliCommand::Generate(args) => {
                let toml_string = toml::to_string_pretty(&config)?;
                if let Some(output) = args.output {
                    std::fs::write(output, toml_string)?;
                } else {
                    println!("{}", toml_string);
                }
                return Ok(());
            }
        }
    }

    let mut radix = RattanRadix::<AfPacketDriver>::new(config)?;
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
