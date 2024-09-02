use std::{
    borrow::Cow,
    collections::HashMap,
    path::PathBuf,
    process::{ExitCode, Stdio, Termination},
    sync::{atomic::AtomicBool, Arc},
};

use clap::{command, Args, Parser, Subcommand, ValueEnum};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use once_cell::sync::OnceCell;
use paste::paste;
use rattan_core::devices::bandwidth::queue::{
    CoDelQueueConfig, DropHeadQueueConfig, DropTailQueueConfig, InfiniteQueueConfig,
};
use rattan_core::devices::bandwidth::BwDeviceConfig;
use rattan_core::devices::StdPacket;
use rattan_core::env::{StdNetEnvConfig, StdNetEnvMode};
use rattan_core::metal::io::af_packet::AfPacketDriver;
use rattan_core::netem_trace::{Bandwidth, Delay};
use rattan_core::radix::RattanRadix;
use rattan_core::{
    config::{
        BwDeviceBuildConfig, BwReplayDeviceBuildConfig, BwReplayQueueConfig,
        DelayDeviceBuildConfig, DeviceBuildConfig, LossDeviceBuildConfig, RattanConfig,
        RattanResourceConfig,
    },
    radix::TaskResultNotify,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};
use tracing_subscriber::Layer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[cfg(feature = "http")]
use rattan_core::control::http::HttpConfig;

// mod docker;

// const CONFIG_PORT_BASE: u16 = 8086;

static LEFT_PID: OnceCell<i32> = OnceCell::new();
static RIGHT_PID: OnceCell<i32> = OnceCell::new();

#[derive(Debug, Parser, Clone)]
#[command(rename_all = "kebab-case")]
#[command(version, propagate_version = true)]
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

    /// Mode to run in
    #[arg(short, long, value_enum, default_value_t = Mode::Compatible)]
    mode: Mode,

    #[command(subcommand)]
    subcommand: Option<CliCommand>,

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
    #[arg(long, global = true, value_name = "Bandwidth", group = "uplink-bw", value_parser = human_bandwidth::parse_bandwidth)]
    uplink_bandwidth: Option<Bandwidth>,
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
    #[arg(long, global = true, value_name = "Bandwidth", group = "downlink-bw", value_parser = human_bandwidth::parse_bandwidth)]
    downlink_bandwidth: Option<Bandwidth>,
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

    #[cfg(feature = "http")]
    /// Enable HTTP control server (overwrite config)
    #[arg(long)]
    http: bool,
    #[cfg(feature = "http")]
    /// HTTP control server port (overwrite config) (default: 8086)
    #[arg(short, long, value_name = "Port")]
    port: Option<u16>,

    /// The file to store compressed packet log (overwrite config) (default: None)
    #[arg(long, value_name = "Packet Log File")]
    packet_log: Option<PathBuf>,

    /// Enable logging to file
    #[arg(long)]
    file_log: bool,
    // This "requires" field uses an underscore '_' instead of a dash '-' since "file_log" is a
    // field name instead of a group name
    /// File log path, default to $CACHE_DIR/rattan/core.log
    #[arg(long, value_name = "Log File", requires = "file_log")]
    file_log_path: Option<String>,

    /// Command to run in left ns. Only used when in isolated mode
    #[arg(long = "left", num_args = 0..)]
    left_command: Option<Vec<String>>,
    /// Command to run in right ns. Only used when in isolated mode
    #[arg(long = "right", num_args = 0..)]
    right_command: Option<Vec<String>>,

    /// Shell used to run if no command is specified. Only used when in compatible mode
    #[arg(short, long, value_enum, default_value_t = TaskShell::Default)]
    shell: TaskShell,
    /// Command to run. Only used when in compatible mode
    #[arg(last = true)]
    command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCommands {
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub left: Option<Vec<String>>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub right: Option<Vec<String>>,
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

#[derive(Debug, Serialize, Deserialize, Clone, ValueEnum)]
pub enum TaskShell {
    Default,
    Sh,
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, Serialize, Deserialize, Clone, ValueEnum, Copy)]
pub enum Mode {
    Compatible,
    Isolated,
    Container,
}

#[derive(ValueEnum, Clone, Debug)]
#[value(rename_all = "lower")]
enum QueueType {
    Infinite,
    DropTail,
    DropHead,
    CoDel,
}

impl TaskShell {
    fn shell(&self) -> Cow<'_, str> {
        match self {
            TaskShell::Default => {
                let shell = std::env::var("SHELL").unwrap_or("/bin/sh".to_string());
                Cow::Owned(shell)
            }
            TaskShell::Sh => Cow::Borrowed("/bin/sh"),
            TaskShell::Bash => Cow::Borrowed("/bin/bash"),
            TaskShell::Zsh => Cow::Borrowed("/bin/zsh"),
            TaskShell::Fish => Cow::Borrowed("/bin/fish"),
        }
    }
}

impl From<Mode> for StdNetEnvMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Compatible => StdNetEnvMode::Compatible,
            Mode::Isolated => StdNetEnvMode::Isolated,
            Mode::Container => StdNetEnvMode::Container,
        }
    }
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
                    return Err(rattan_core::error::Error::ConfigError(format!("Failed to parse queue args {:?}: {}", $q_args, e)));
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
                    return Err(rattan_core::error::Error::ConfigError(format!("Failed to parse queue args {:?}: {}", $q_args, e)));
                }
            }
        )
    };
}

fn main() -> ExitCode {
    // Parse Arguments
    let mut opts = Arguments::parse();
    debug!("{:?}", opts);
    // if opts.docker {
    //     docker::docker_main(opts).unwrap();
    //     return;
    // }

    // Install Tracing Subscriber
    let subscriber =
        tracing_subscriber::registry().with(tracing_subscriber::fmt::layer().with_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        ));
    let _guard: Option<_> = if opts.file_log {
        if let Some((log_dir, file_name)) = opts
            .file_log_path
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
                    .map(|log_dir| (log_dir, "core.log".to_string()))
            })
        {
            if let Err(e) = std::fs::create_dir_all(&log_dir) {
                error!("Failed to create log directory: {:?}", e);
                return ExitCode::from(74);
            }
            let file_logger = match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join(file_name))
            {
                Ok(f) => f,
                Err(e) => {
                    error!("Failed to open log file: {:?}", e);
                    return ExitCode::from(74);
                }
            };
            // let file_logger = tracing_appender::rolling::daily(log_dir, file_name_prefix);
            let (non_blocking, guard) = tracing_appender::non_blocking(file_logger);
            let env_filter = tracing_subscriber::EnvFilter::try_from_env("RATTAN_LOG")
                .unwrap_or_else(|_| "info".into());
            subscriber
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
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

    let left_handle_finished = Arc::new(AtomicBool::new(false));
    let left_handle_finished_inner = left_handle_finished.clone();
    let right_handle_finished = Arc::new(AtomicBool::new(false));
    let right_handle_finished_inner = right_handle_finished.clone();
    // Install Ctrl-C Handler
    ctrlc::set_handler(move || {
        if !left_handle_finished_inner.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(pid) = LEFT_PID.get() {
                let _ = signal::kill(Pid::from_raw(*pid), Signal::SIGTERM).inspect_err(|e| {
                    tracing::error!("Failed to send SIGTERM to left task: {}", e);
                });
            }
        }
        if !right_handle_finished_inner.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(pid) = RIGHT_PID.get() {
                let _ = signal::kill(Pid::from_raw(*pid), Signal::SIGTERM).inspect_err(|e| {
                    tracing::error!("Failed to send SIGTERM to right task: {}", e);
                });
            }
        }
    })
    .expect("unable to install ctrl+c handler");

    // Main CLI
    let main_cli = || -> rattan_core::error::Result<()> {
        let mut config = match opts.config {
            Some(ref config_file) => {
                info!("Loading config from {}", config_file);
                if !std::path::Path::new(config_file).exists() {
                    tracing::warn!("Config file {} specified but does not exist", config_file);
                }
                let config: RattanConfig<StdPacket> = Figment::new()
                    .merge(Toml::file(config_file))
                    .merge(Env::prefixed("RATTAN_"))
                    .extract()
                    .map_err(|e| rattan_core::error::Error::ConfigError(e.to_string()))?;

                config
            }
            None => {
                let mut devices_config = HashMap::<String, DeviceBuildConfig<StdPacket>>::new();
                let mut links_config = HashMap::<String, String>::new();
                let mut uplink_count = 0;
                let mut downlink_count = 0;

                if let Some(bandwidth) = opts.uplink_bandwidth {
                    let device_config = match opts.uplink_queue {
                        Some(QueueType::Infinite) | None => {
                            DeviceBuildConfig::Bw(BwDeviceBuildConfig::Infinite(
                                BwDeviceConfig::new(bandwidth, InfiniteQueueConfig::new(), None),
                            ))
                        }
                        Some(QueueType::DropTail) => {
                            bw_q_args_into_config!(
                                DropTail,
                                opts.uplink_queue_args.clone(),
                                bandwidth
                            )
                        }
                        Some(QueueType::DropHead) => {
                            bw_q_args_into_config!(
                                DropHead,
                                opts.uplink_queue_args.clone(),
                                bandwidth
                            )
                        }
                        Some(QueueType::CoDel) => {
                            bw_q_args_into_config!(CoDel, opts.uplink_queue_args.clone(), bandwidth)
                        }
                    };
                    uplink_count += 1;
                    devices_config.insert(format!("up_{}", uplink_count), device_config);
                } else if let Some(trace_file) = opts.uplink_trace {
                    let device_config = match opts.uplink_queue {
                        Some(QueueType::Infinite) | None => DeviceBuildConfig::BwReplay(
                            BwReplayDeviceBuildConfig::Infinite(BwReplayQueueConfig::new(
                                trace_file,
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
                    devices_config.insert(format!("up_{}", uplink_count), device_config);
                }

                if let Some(bandwidth) = opts.downlink_bandwidth {
                    let device_config = match opts.downlink_queue {
                        Some(QueueType::Infinite) | None => {
                            DeviceBuildConfig::Bw(BwDeviceBuildConfig::Infinite(
                                BwDeviceConfig::new(bandwidth, InfiniteQueueConfig::new(), None),
                            ))
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
                            bw_q_args_into_config!(
                                CoDel,
                                opts.downlink_queue_args.clone(),
                                bandwidth
                            )
                        }
                    };
                    downlink_count += 1;
                    devices_config.insert(format!("down_{}", downlink_count), device_config);
                } else if let Some(trace_file) = opts.downlink_trace {
                    let device_config = match opts.downlink_queue {
                        Some(QueueType::Infinite) | None => DeviceBuildConfig::BwReplay(
                            BwReplayDeviceBuildConfig::Infinite(BwReplayQueueConfig::new(
                                trace_file,
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
                    devices_config.insert(format!("down_{}", downlink_count), device_config);
                }

                if let Some(delay) = opts.uplink_delay {
                    let device_config =
                        DeviceBuildConfig::Delay(DelayDeviceBuildConfig::new(delay));
                    uplink_count += 1;
                    devices_config.insert(format!("up_{}", uplink_count), device_config);
                }

                if let Some(delay) = opts.downlink_delay {
                    let device_config =
                        DeviceBuildConfig::Delay(DelayDeviceBuildConfig::new(delay));
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
                        mode: opts.mode.into(),
                        client_cores: vec![1],
                        server_cores: vec![3],
                        ..Default::default()
                    },
                    devices: devices_config,
                    links: links_config,
                    resource: RattanResourceConfig {
                        memory: None,
                        cpu: Some(vec![2]),
                    },
                    ..Default::default()
                }
            }
        };

        // Overwrite config with CLI options
        #[cfg(feature = "http")]
        if opts.http {
            config.http.enable = true;
        }
        #[cfg(feature = "http")]
        if let Some(port) = opts.port {
            config.http.port = port;
        }
        if let Some(packet_log) = opts.packet_log {
            config.general.packet_log = Some(packet_log);
        }
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
                    let toml_string = toml::to_string_pretty(&config)
                        .map_err(|e| rattan_core::error::Error::ConfigError(e.to_string()))?;
                    if let Some(output) = args.output {
                        std::fs::write(output, toml_string)?;
                    } else {
                        println!("{}", toml_string);
                    }
                    return Ok(());
                }
            }
        }

        let commands = {
            let config = {
                if let Some(config_file) = opts.config {
                    Figment::new().merge(Toml::file(config_file).nested())
                } else {
                    Figment::new()
                }
            };
            config
                .merge(Serialized::from(
                    TaskCommands {
                        left: opts.left_command.take(),
                        right: opts.right_command.take(),
                    },
                    "commands",
                ))
                .merge(Env::prefixed("RATTAN_").profile("commands"))
                .select("commands")
                .extract::<TaskCommands>()
                .map_err(|e| rattan_core::error::Error::ConfigError(e.to_string()))?
        };

        if config.env.mode == StdNetEnvMode::Container {
            return Err(rattan_core::error::Error::ConfigError(
                "Container mode is not supported yet".to_string(),
            ));
        }
        if config.env.mode == StdNetEnvMode::Isolated
            && (commands.left.is_none() || commands.right.is_none())
        {
            return Err(rattan_core::error::Error::ConfigError(
                "Isolated mode requires commands to be set in both sides".to_string(),
            ));
        }

        let mut radix = RattanRadix::<AfPacketDriver>::new(config)?;
        radix.spawn_rattan()?;
        radix.start_rattan()?;

        let (tx_left, rx) = std::sync::mpsc::channel();
        let tx_right = tx_left.clone();

        match radix.get_mode() {
            StdNetEnvMode::Compatible => {
                let ip_list = radix.right_ip_list();
                let left_handle = radix.left_spawn(None, move || {
                    let mut client_handle = std::process::Command::new("/usr/bin/env");
                    ip_list.iter().enumerate().for_each(|(i, ip)| {
                        client_handle.env(format!("RATTAN_IP_{i}"), ip.to_string());
                    });
                    client_handle.env("RATTAN_EXT", ip_list[0].to_string());
                    client_handle.env("RATTAN_BASE", ip_list[1].to_string());
                    if let Some(arguments) = opts.command {
                        client_handle.args(arguments);
                    } else if let Some(arguments) = commands.left {
                        client_handle.args(arguments);
                    } else {
                        client_handle.arg(opts.shell.shell().as_ref());
                        if opts.shell.shell().ends_with("/bash") {
                            client_handle
                                .env("PROMPT_COMMAND", "PS1=\"[rattan] $PS1\" PROMPT_COMMAND=");
                        }
                    }
                    info!("Running {:?}", client_handle);
                    let mut client_handle = client_handle
                        .stdin(Stdio::inherit())
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .spawn()?;
                    let pid = client_handle.id() as i32;
                    debug!("Left pid: {}", pid);
                    let _ = LEFT_PID.set(pid);
                    let status = client_handle.wait()?;
                    left_handle_finished.store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok(status)
                })?;
                match left_handle.join() {
                    Ok(Ok(status)) => {
                        if let Some(code) = status.code() {
                            if code == 0 {
                                info!("Left handle {}", status);
                                Ok(())
                            } else {
                                Err(rattan_core::error::Error::Custom(format!(
                                    "Left handle {}",
                                    status
                                )))
                            }
                        } else {
                            Err(rattan_core::error::Error::Custom(format!(
                                "Left handle {}",
                                status
                            )))
                        }
                    }
                    Ok(Err(e)) => Err(rattan_core::error::Error::Custom(format!(
                        "Left handle exited with error: {:?}",
                        e
                    ))),
                    Err(e) => Err(rattan_core::error::Error::Custom(format!(
                        "Fail to join left handle: {:?}",
                        e
                    ))),
                }
            }
            StdNetEnvMode::Isolated => {
                let ip_list = radix.left_ip_list();
                let right_handle = radix.right_spawn(Some(tx_right), move || {
                    let mut server_handle = std::process::Command::new("/usr/bin/env");
                    ip_list.iter().enumerate().for_each(|(i, ip)| {
                        server_handle.env(format!("RATTAN_IP_{i}"), ip.to_string());
                    });
                    server_handle.env("RATTAN_EXT", ip_list[0].to_string());
                    server_handle.env("RATTAN_BASE", ip_list[1].to_string());
                    if let Some(arguments) = commands.right {
                        server_handle.args(arguments);
                    }
                    info!("Running {:?}", server_handle);
                    let mut server_handle = server_handle
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()?;
                    let pid = server_handle.id() as i32;
                    debug!("Right pid: {}", pid);
                    let _ = RIGHT_PID.set(pid);
                    let status = server_handle.wait()?;
                    right_handle_finished.store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok(status)
                })?;
                let ip_list = radix.right_ip_list();
                let left_handle = radix.left_spawn(Some(tx_left), move || {
                    let mut client_handle = std::process::Command::new("/usr/bin/env");
                    ip_list.iter().enumerate().for_each(|(i, ip)| {
                        client_handle.env(format!("RATTAN_IP_{i}"), ip.to_string());
                    });
                    client_handle.env("RATTAN_EXT", ip_list[0].to_string());
                    client_handle.env("RATTAN_BASE", ip_list[1].to_string());
                    if let Some(arguments) = commands.left {
                        client_handle.args(arguments);
                    }
                    info!("Running {:?}", client_handle);
                    let mut client_handle = client_handle
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()?;
                    let pid = client_handle.id() as i32;
                    debug!("Left pid: {}", pid);
                    let _ = LEFT_PID.set(pid);
                    let status = client_handle.wait()?;
                    left_handle_finished.store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok(status)
                })?;
                let mut left_res = Ok(());
                let mut right_res = Ok(());
                match rx.recv() {
                    Ok(notify) => match notify {
                        TaskResultNotify::Left => {
                            match left_handle.join() {
                                Ok(Ok(status)) => {
                                    if let Some(code) = status.code() {
                                        if code == 0 {
                                            info!("Left handle {}", status);
                                        } else {
                                            left_res = left_res.and_then(|_| {
                                                Err(format!("Left handle {}", status))
                                            });
                                        }
                                    } else {
                                        left_res = left_res
                                            .and_then(|_| Err(format!("Left handle {}", status)));
                                    }
                                }
                                Ok(Err(e)) => {
                                    left_res = left_res.and_then(|_| {
                                        Err(format!("Left handle exited with error: {:?}", e))
                                    });
                                    // TODO: we may also add other arguments to disable the killing of the other half
                                    if right_handle.is_finished() {
                                        warn!("Right handle is already finished");
                                    } else {
                                        warn!("Try to send SIGTERM to right spawned thread");
                                        if let Some(pid) = RIGHT_PID.get() {
                                            let _ =
                                                signal::kill(Pid::from_raw(*pid), Signal::SIGTERM)
                                                    .inspect_err(|e| {
                                                        tracing::error!(
                                                            "Failed to send SIGTERM: {}",
                                                            e
                                                        );
                                                    });
                                        }
                                        // TODO: we may wait for a while before sending SIGKILL
                                    }
                                }
                                Err(e) => {
                                    left_res = left_res.and_then(|_| {
                                        Err(format!("Fail to join left handle: {:?}", e))
                                    });
                                }
                            }
                            match right_handle.join() {
                                Ok(Ok(status)) => {
                                    if let Some(code) = status.code() {
                                        if code == 0 {
                                            info!("Right handle {}", status);
                                        } else {
                                            right_res = right_res.and_then(|_| {
                                                Err(format!("Right handle {}", status))
                                            });
                                        }
                                    } else {
                                        right_res = right_res
                                            .and_then(|_| Err(format!("Right handle {}", status)));
                                    }
                                }
                                Ok(Err(e)) => {
                                    right_res = right_res.and_then(|_| {
                                        Err(format!("Right handle exited with error: {:?}", e))
                                    });
                                }
                                Err(e) => {
                                    right_res = right_res.and_then(|_| {
                                        Err(format!("Fail to join right handle: {:?}", e))
                                    });
                                }
                            }
                        }
                        TaskResultNotify::Right => {
                            match right_handle.join() {
                                Ok(Ok(status)) => {
                                    if let Some(code) = status.code() {
                                        if code == 0 {
                                            info!("Right handle {}", status);
                                        } else {
                                            right_res = right_res.and_then(|_| {
                                                Err(format!("Right handle {}", status))
                                            });
                                        }
                                    } else {
                                        right_res = right_res
                                            .and_then(|_| Err(format!("Right handle {}", status)));
                                    }
                                }
                                Ok(Err(e)) => {
                                    right_res = right_res.and_then(|_| {
                                        Err(format!("Right handle exited with error: {:?}", e))
                                    });
                                    // TODO: we may also add other arguments to disable the killing of the other half
                                    if left_handle.is_finished() {
                                        warn!("Left handle is already finished");
                                    } else {
                                        warn!("Try to send SIGTERM to left spawned thread");
                                        if let Some(pid) = LEFT_PID.get() {
                                            let _ =
                                                signal::kill(Pid::from_raw(*pid), Signal::SIGTERM)
                                                    .inspect_err(|e| {
                                                        tracing::error!(
                                                            "Failed to send SIGTERM: {}",
                                                            e
                                                        );
                                                    });
                                        }
                                        // TODO: we may wait for a while before sending SIGKILL
                                    }
                                }
                                Err(e) => {
                                    right_res = right_res.and_then(|_| {
                                        Err(format!("Fail to join right handle: {:?}", e))
                                    });
                                }
                            }
                            match left_handle.join() {
                                Ok(Ok(status)) => {
                                    if let Some(code) = status.code() {
                                        if code == 0 {
                                            info!("Left handle {}", status);
                                        } else {
                                            left_res = left_res.and_then(|_| {
                                                Err(format!("Left handle {}", status))
                                            });
                                        }
                                    } else {
                                        left_res = left_res
                                            .and_then(|_| Err(format!("Left handle {}", status)));
                                    }
                                }
                                Ok(Err(e)) => {
                                    left_res = left_res.and_then(|_| {
                                        Err(format!("Left handle exited with error: {:?}", e))
                                    });
                                }
                                Err(e) => {
                                    left_res = left_res.and_then(|_| {
                                        Err(format!("Fail to join left handle: {:?}", e))
                                    });
                                }
                            }
                        }
                    },
                    Err(e) => {
                        return Err(rattan_core::error::Error::ChannelError(format!(
                            "Fail to receive from channel: {:?}",
                            e
                        )));
                    }
                }
                match (left_res, right_res) {
                    (Ok(()), Ok(())) => Ok(()),
                    (Err(e), Ok(())) => Err(rattan_core::error::Error::Custom(e)),
                    (Ok(()), Err(e)) => Err(rattan_core::error::Error::Custom(e)),
                    (Err(e1), Err(e2)) => {
                        Err(rattan_core::error::Error::Custom(format!("{}. {}", e1, e2)))
                    }
                }
            }
            StdNetEnvMode::Container => Ok(()),
        }

        // get the last byte of rattan_base as the port number
        // let port = CONFIG_PORT_BASE - 1
        //     + match rattan_base {
        //         std::net::IpAddr::V4(ip) => ip.octets()[3],
        //         std::net::IpAddr::V6(ip) => ip.octets()[15],
        //     } as u16;
        // let config = RattanMachineConfig { original_ns, port };
    };
    match main_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{}", e);
            e.report()
        }
    }
}
