use std::{
    borrow::Cow,
    fs::File,
    path::PathBuf,
    process::{ExitCode, Stdio, Termination},
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use clap::{command, Args, Parser, Subcommand, ValueEnum};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use once_cell::sync::OnceCell;

use rattan_core::cells::StdPacket;
use rattan_core::env::StdNetEnvMode;
use rattan_core::metal::io::af_packet::AfPacketDriver;
use rattan_core::radix::RattanRadix;
use rattan_core::{config::RattanConfig, radix::TaskResultNotify};
use serde::{Deserialize, Serialize};
use tracing::warn;
use tracing_subscriber::Layer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    build::CLAP_LONG_VERSION, combined_trace::write_combined_trace, log_converter::LogConverterArgs,
};
use shadow_rs::shadow;

mod channel;
mod combined_trace;
mod log_converter;
// mod docker;

// const CONFIG_PORT_BASE: u16 = 8086;

shadow!(build);

static LEFT_PID: OnceCell<i32> = OnceCell::new();
static RIGHT_PID: OnceCell<i32> = OnceCell::new();

fn parse_duration(delay: &str) -> Result<Duration, jiff::Error> {
    let span: jiff::Span = delay.parse()?;
    Duration::try_from(span)
}

#[derive(Debug, Parser, Clone)]
#[command(rename_all = "kebab-case")]
#[command(version, propagate_version = true, long_version = CLAP_LONG_VERSION)]
pub struct Arguments {
    // Verbose debug output
    // #[arg(short, long)]
    // verbose: bool,
    // Run in docker mode
    // #[arg(long)]
    // docker: bool,
    #[command(subcommand)]
    subcommand: CliCommand,

    /// Generate config file instead of running a instance
    ///
    /// If this flag is set, the program will only generate the config to stdout and exit.
    #[arg(long, global = true)]
    generate: bool,

    /// Generate combined trace until `combined_trace` since the trace starts
    ///
    /// If this is set, the program will only generate the csv to stdout and exit.
    #[arg(
        long,
        global = true,
        value_parser = parse_duration
    )]
    combined_trace: Option<Duration>,

    /// Generate combined trace csv file to the specified path instead of stdout
    #[arg(long, requires = "combined_trace", global = true, value_name = "File")]
    combined_trace_path: Option<PathBuf>,

    /// Used in isolated mode only. If set, stdout of left is passed to output of this program.
    #[arg(long, global = true)]
    left_stdout: bool,

    /// Used in isolated mode only. If set, stdout of rihgt is passed to output of this program.
    #[arg(long, global = true)]
    right_stdout: bool,

    /// Used in isolated mode only. If set, stderr of left is passed to output of this program.
    #[arg(long, global = true)]
    left_stderr: bool,

    /// Used in isolated mode only. If set, stderr of right is passed to output of this program.
    #[arg(long, global = true)]
    right_stderr: bool,

    /// Generate config file to the specified path instead of stdout
    #[arg(long, requires = "generate", global = true, value_name = "File")]
    generate_path: Option<PathBuf>,

    #[cfg(feature = "http")]
    /// Enable HTTP control server (overwrite config)
    #[arg(long, global = true)]
    http: bool,
    #[cfg(feature = "http")]
    /// HTTP control server port (overwrite config) (default: 8086)
    #[arg(short, long, value_name = "Port", global = true)]
    port: Option<u16>,

    /// The file to store compressed packet log (overwrite config) (default: None)
    #[arg(long, value_name = "File", global = true)]
    packet_log: Option<PathBuf>,

    /// Enable logging to file
    #[arg(long, global = true)]
    file_log: bool,
    // This "requires" field uses an underscore '_' instead of a dash '-' since "file_log" is a
    // field name instead of a group name
    /// File log path, default to $CACHE_DIR/rattan/core.log
    #[arg(long, value_name = "File", requires = "file_log", global = true)]
    file_log_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCommands {
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub left: Option<Vec<String>>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub right: Option<Vec<String>>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub shell: Option<TaskShell>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayTaskCommands {
    pub commands: TaskCommands,
}

#[derive(Subcommand, Debug, Clone)]
enum CliCommand {
    /// Run a templated channel with command line arguments.
    Link(channel::ChannelArgs),
    /// Run the instance according to the config.
    Run(RunArgs),
    /// Convert Rattan packet log into .pcapng file.
    Convert(LogConverterArgs),
}

#[derive(Args, Debug, Default, Clone)]
#[command(rename_all = "kebab-case")]
pub struct RunArgs {
    /// Use config file to run a instance.
    #[arg(short, long, value_name = "Config File")]
    pub config: PathBuf,
    /// Command to run in left ns. Can be used in compatible and isolated mode
    #[arg(long = "left", num_args = 0..)]
    left_command: Option<Vec<String>>,
    /// Command to run in right ns. Only used in isolated mode
    #[arg(long = "right", num_args = 0..)]
    right_command: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, ValueEnum, Copy, Default)]
pub enum TaskShell {
    #[default]
    Default,
    Sh,
    Bash,
    Zsh,
    Fish,
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

fn main() -> ExitCode {
    // Parse Arguments
    let opts = Arguments::parse();
    tracing::debug!("{:?}", opts);
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
                tracing::error!("Failed to create log directory: {:?}", e);
                return ExitCode::from(74);
            }
            let file_logger = match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join(file_name))
            {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!("Failed to open log file: {:?}", e);
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
        let (mut config, commands) = match opts.subcommand {
            CliCommand::Run(mut args) => {
                tracing::info!("Loading config from {}", args.config.display());
                if !args.config.exists() {
                    return Err(rattan_core::error::Error::ConfigError(format!(
                        "Config file {} does not exist",
                        args.config.display()
                    )));
                }
                let config: RattanConfig<StdPacket> = Figment::new()
                    .merge(Toml::file(&args.config))
                    .merge(Env::prefixed("RATTAN_"))
                    .extract()
                    .map_err(|e| rattan_core::error::Error::ConfigError(e.to_string()))?;
                let commands = Figment::new()
                    .merge(Toml::file(args.config).nested())
                    .merge(Serialized::from(
                        TaskCommands {
                            left: args.left_command.take(),
                            right: args.right_command.take(),
                            shell: None,
                        },
                        "commands",
                    ))
                    .merge(Env::prefixed("RATTAN_").profile("commands"))
                    .select("commands")
                    .extract::<TaskCommands>()
                    .map_err(|e| rattan_core::error::Error::ConfigError(e.to_string()))?;
                (config, commands)
            }
            CliCommand::Link(mut args) => {
                let commands = Figment::new()
                    .merge(Serialized::from(
                        TaskCommands {
                            left: args.command.take(),
                            right: None,
                            shell: Some(args.shell),
                        },
                        "commands",
                    ))
                    .merge(Env::prefixed("RATTAN_").profile("commands"))
                    .select("commands")
                    .extract::<TaskCommands>()
                    .map_err(|e| rattan_core::error::Error::ConfigError(e.to_string()))?;
                let config = args.build_rattan_config()?;
                (config, commands)
            }
            CliCommand::Convert(args) => {
                log_converter::convert_log_to_pcapng(args.input, args.output)?;
                return Ok(());
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
        tracing::debug!(?config);

        // Generate config
        if opts.generate {
            // DONE: generate commands as well
            let mut toml_string = toml::to_string_pretty(&config)
                .map_err(|e| rattan_core::error::Error::ConfigError(e.to_string()))?;
            let display_commands = DisplayTaskCommands { commands };
            let command_string = toml::to_string_pretty(&display_commands)
                .map_err(|e| rattan_core::error::Error::ConfigError(e.to_string()))?;

            toml_string.push_str(format!("\n{command_string}").as_str());
            if let Some(output) = opts.generate_path {
                std::fs::write(output, toml_string)?;
            } else {
                println!("{toml_string}");
            }
            return Ok(());
        }

        if let Some(combined_trace_length) = opts.combined_trace {
            if let Some(output_path) = opts.combined_trace_path {
                return write_combined_trace(
                    File::create(output_path)?,
                    config.cells,
                    combined_trace_length,
                );
            } else {
                return write_combined_trace(
                    std::io::stdout(),
                    config.cells,
                    combined_trace_length,
                );
            }
        }

        // Check if the config can correctly spawn
        if config.cells.is_empty() {
            tracing::warn!("No cells specified in config");
        }
        if config.links.is_empty() {
            tracing::warn!("No links specified in config");
        }
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

        // Start Rattan
        let mut radix = RattanRadix::<AfPacketDriver>::new(config)?;
        radix.spawn_rattan()?;
        radix.start_rattan()?;

        let (tx_left, rx) = std::sync::mpsc::channel();
        let tx_right = tx_left.clone();

        match radix.get_mode() {
            StdNetEnvMode::Compatible => {
                if opts.left_stdout | opts.left_stderr | opts.right_stdout | opts.right_stderr {
                    warn!(
                        "--left-stdout, --left-stderr, --right-stdout, --right-stderr \
                        are for isolated mode only and thus ignored."
                    );
                }

                let ip_list = radix.right_ip_list();
                let left_handle = radix.left_spawn(None, move || {
                    let mut client_handle = std::process::Command::new("/usr/bin/env");
                    ip_list.iter().enumerate().for_each(|(i, ip)| {
                        client_handle.env(format!("RATTAN_IP_{i}"), ip.to_string());
                    });
                    client_handle.env("RATTAN_EXT", ip_list[0].to_string());
                    client_handle.env("RATTAN_BASE", ip_list[1].to_string());
                    if let Some(arguments) = commands.left {
                        client_handle.args(arguments);
                    } else {
                        let shell = commands.shell.unwrap_or_default();
                        client_handle.arg(shell.shell().as_ref());
                        if shell.shell().ends_with("/bash") {
                            client_handle
                                .env("PROMPT_COMMAND", "PS1=\"[rattan] $PS1\" PROMPT_COMMAND=");
                        }
                    }
                    tracing::info!("Running {:?}", client_handle);
                    let mut client_handle = client_handle
                        .stdin(Stdio::inherit())
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .spawn()?;
                    let pid = client_handle.id() as i32;
                    tracing::debug!("Left pid: {}", pid);
                    let _ = LEFT_PID.set(pid);
                    let status = client_handle.wait()?;
                    left_handle_finished.store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok(status)
                })?;
                match left_handle.join() {
                    Ok(Ok(status)) => {
                        if let Some(code) = status.code() {
                            if code == 0 {
                                tracing::info!("Left handle {status}");
                                Ok(())
                            } else {
                                Err(rattan_core::error::Error::Custom(format!(
                                    "Left handle {status}"
                                )))
                            }
                        } else {
                            Err(rattan_core::error::Error::Custom(format!(
                                "Left handle {status}"
                            )))
                        }
                    }
                    Ok(Err(e)) => Err(rattan_core::error::Error::Custom(format!(
                        "Left handle exited with error: {e:?}"
                    ))),
                    Err(e) => Err(rattan_core::error::Error::Custom(format!(
                        "Fail to join left handle: {e:?}"
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
                    tracing::info!("Running in right NS {server_handle:?}");
                    let mut server_handle = server_handle
                        .stdin(Stdio::null())
                        .stdout(if opts.right_stdout {
                            Stdio::inherit()
                        } else {
                            Stdio::null()
                        })
                        .stderr(if opts.right_stderr {
                            Stdio::inherit()
                        } else {
                            Stdio::null()
                        })
                        .spawn()?;
                    let pid = server_handle.id() as i32;
                    tracing::debug!("Right pid: {pid}");
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
                    tracing::info!("Running in left NS {client_handle:?}");
                    let mut client_handle = client_handle
                        .stdin(Stdio::null())
                        .stdout(if opts.left_stdout {
                            Stdio::inherit()
                        } else {
                            Stdio::null()
                        })
                        .stderr(if opts.left_stderr {
                            Stdio::inherit()
                        } else {
                            Stdio::null()
                        })
                        .spawn()?;
                    let pid = client_handle.id() as i32;
                    tracing::debug!("Left pid: {pid}");
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
                                            tracing::info!("Left handle {status}");
                                        } else {
                                            left_res = left_res
                                                .and_then(|_| Err(format!("Left handle {status}")));
                                        }
                                    } else {
                                        left_res = left_res
                                            .and_then(|_| Err(format!("Left handle {status}")));
                                    }
                                }
                                Ok(Err(e)) => {
                                    left_res = left_res.and_then(|_| {
                                        Err(format!("Left handle exited with error: {e:?}"))
                                    });
                                    // TODO: we may also add other arguments to disable the killing of the other half
                                    if right_handle.is_finished() {
                                        tracing::warn!("Right handle is already finished");
                                    } else {
                                        tracing::warn!(
                                            "Try to send SIGTERM to right spawned thread"
                                        );
                                        if let Some(pid) = RIGHT_PID.get() {
                                            let _ =
                                                signal::kill(Pid::from_raw(*pid), Signal::SIGTERM)
                                                    .inspect_err(|e| {
                                                        tracing::error!(
                                                            "Failed to send SIGTERM: {e}"
                                                        );
                                                    });
                                        }
                                        // TODO: we may wait for a while before sending SIGKILL
                                    }
                                }
                                Err(e) => {
                                    left_res = left_res.and_then(|_| {
                                        Err(format!("Fail to join left handle: {e:?}"))
                                    });
                                }
                            }
                            match right_handle.join() {
                                Ok(Ok(status)) => {
                                    if let Some(code) = status.code() {
                                        if code == 0 {
                                            tracing::info!("Right handle {status}");
                                        } else {
                                            right_res = right_res.and_then(|_| {
                                                Err(format!("Right handle {status}"))
                                            });
                                        }
                                    } else {
                                        right_res = right_res
                                            .and_then(|_| Err(format!("Right handle {status}")));
                                    }
                                }
                                Ok(Err(e)) => {
                                    right_res = right_res.and_then(|_| {
                                        Err(format!("Right handle exited with error: {e:?}"))
                                    });
                                }
                                Err(e) => {
                                    right_res = right_res.and_then(|_| {
                                        Err(format!("Fail to join right handle: {e:?}"))
                                    });
                                }
                            }
                        }
                        TaskResultNotify::Right => {
                            match right_handle.join() {
                                Ok(Ok(status)) => {
                                    if let Some(code) = status.code() {
                                        if code == 0 {
                                            tracing::info!("Right handle {status}");
                                        } else {
                                            right_res = right_res.and_then(|_| {
                                                Err(format!("Right handle {status}"))
                                            });
                                        }
                                    } else {
                                        right_res = right_res
                                            .and_then(|_| Err(format!("Right handle {status}")));
                                    }
                                }
                                Ok(Err(e)) => {
                                    right_res = right_res.and_then(|_| {
                                        Err(format!("Right handle exited with error: {e:?}"))
                                    });
                                    // TODO: we may also add other arguments to disable the killing of the other half
                                    if left_handle.is_finished() {
                                        tracing::warn!("Left handle is already finished");
                                    } else {
                                        tracing::warn!(
                                            "Try to send SIGTERM to left spawned thread"
                                        );
                                        if let Some(pid) = LEFT_PID.get() {
                                            let _ =
                                                signal::kill(Pid::from_raw(*pid), Signal::SIGTERM)
                                                    .inspect_err(|e| {
                                                        tracing::error!(
                                                            "Failed to send SIGTERM: {e}"
                                                        );
                                                    });
                                        }
                                        // TODO: we may wait for a while before sending SIGKILL
                                    }
                                }
                                Err(e) => {
                                    right_res = right_res.and_then(|_| {
                                        Err(format!("Fail to join right handle: {e:?}"))
                                    });
                                }
                            }
                            match left_handle.join() {
                                Ok(Ok(status)) => {
                                    if let Some(code) = status.code() {
                                        if code == 0 {
                                            tracing::info!("Left handle {status}");
                                        } else {
                                            left_res = left_res
                                                .and_then(|_| Err(format!("Left handle {status}")));
                                        }
                                    } else {
                                        left_res = left_res
                                            .and_then(|_| Err(format!("Left handle {status}")));
                                    }
                                }
                                Ok(Err(e)) => {
                                    left_res = left_res.and_then(|_| {
                                        Err(format!("Left handle exited with error: {e:?}"))
                                    });
                                }
                                Err(e) => {
                                    left_res = left_res.and_then(|_| {
                                        Err(format!("Fail to join left handle: {e:?}"))
                                    });
                                }
                            }
                        }
                    },
                    Err(e) => {
                        return Err(rattan_core::error::Error::ChannelError(format!(
                            "Fail to receive from channel: {e:?}"
                        )));
                    }
                }
                match (left_res, right_res) {
                    (Ok(()), Ok(())) => Ok(()),
                    (Err(e), Ok(())) => Err(rattan_core::error::Error::Custom(e)),
                    (Ok(()), Err(e)) => Err(rattan_core::error::Error::Custom(e)),
                    (Err(e1), Err(e2)) => {
                        Err(rattan_core::error::Error::Custom(format!("{e1}. {e2}")))
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
            tracing::error!("{e}");
            e.report()
        }
    }
}
