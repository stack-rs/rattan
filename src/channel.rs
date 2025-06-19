use std::collections::HashMap;

use clap::{Args, ValueEnum};
use netem_trace::{Bandwidth, Delay};
use paste::paste;
use rattan_core::{
    cells::{
        bandwidth::{
            queue::{
                CoDelQueueConfig, DropHeadQueueConfig, DropTailQueueConfig, InfiniteQueueConfig,
            },
            BwCellConfig,
        },
        StdPacket,
    },
    config::{
        BwCellBuildConfig, BwReplayCellBuildConfig, BwReplayQueueConfig, CellBuildConfig,
        DelayCellBuildConfig, LossCellBuildConfig, RattanConfig, RattanResourceConfig,
    },
    env::{StdNetEnvConfig, StdNetEnvMode},
};

use crate::TaskShell;

#[derive(Debug, Args, Clone)]
#[command(rename_all = "kebab-case")]
#[command(version, propagate_version = true)]
pub struct ChannelArgs {
    /// Uplink packet loss
    #[arg(long, value_name = "Loss")]
    uplink_loss: Option<f64>,
    /// Downlink packet loss
    #[arg(long, value_name = "Loss")]
    downlink_loss: Option<f64>,

    /// Uplink delay
    #[arg(long, value_name = "Delay", value_parser = parse_delay)]
    uplink_delay: Option<Delay>,
    /// Downlink delay
    #[arg(long, value_name = "Delay", value_parser = parse_delay)]
    downlink_delay: Option<Delay>,

    /// Uplink bandwidth
    #[arg(long, value_name = "Bandwidth", group = "uplink-bw", value_parser = human_bandwidth::parse_bandwidth)]
    uplink_bandwidth: Option<Bandwidth>,
    /// Uplink trace file
    #[arg(long, value_name = "File", group = "uplink-bw")]
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
    #[arg(long, value_name = "JSON", requires = "uplink-queue")]
    uplink_queue_args: Option<String>,

    /// Downlink bandwidth
    #[arg(long, value_name = "Bandwidth", group = "downlink-bw", value_parser = human_bandwidth::parse_bandwidth)]
    downlink_bandwidth: Option<Bandwidth>,
    /// Downlink trace file
    #[arg(long, value_name = "File", group = "downlink-bw")]
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

    /// Shell used to run if no command is specified. Only used when in compatible mode
    #[arg(short, long, value_enum, default_value_t = TaskShell::Default)]
    pub shell: TaskShell,
    /// Command to run. Only used when in compatible mode
    #[arg(last = true)]
    pub command: Option<Vec<String>>,
}

fn parse_delay(delay: &str) -> Result<Delay, jiff::Error> {
    let span: jiff::Span = delay.parse()?;
    Delay::try_from(span)
}

#[derive(ValueEnum, Clone, Debug)]
#[value(rename_all = "lower")]
enum QueueType {
    Infinite,
    DropTail,
    DropHead,
    CoDel,
}

// Deserialize queue args and create BwCellBuildConfig
// $q_type: queue type (key in `QueueType`)
// $q_args: queue args
// $bw: bandwidth for BwCell
macro_rules! bw_q_args_into_config {
    ($q_type:ident, $q_args:expr, $bw:expr) => {
        paste!(
            match serde_json::from_str::<[<$q_type QueueConfig>]> (&$q_args.unwrap_or("{}".to_string())) {
                Ok(queue_config) => CellBuildConfig::Bw(BwCellBuildConfig::$q_type(
                    if $q_args.is_none() {
                        BwCellConfig::new($bw, None, None)
                    } else {
                        BwCellConfig::new($bw, queue_config, None)
                    }
                )),
                Err(e) => {
                    tracing::error!("Failed to parse queue args {:?}: {}", $q_args, e);
                    return Err(rattan_core::error::Error::ConfigError(format!("Failed to parse queue args {:?}: {}", $q_args, e)));
                }
            }
        )
    };
}

// Deserialize queue args and create BwReplayCellBuildConfig
// $q_type: queue type (key in `QueueType`)
// $q_args: queue args
// $mahimahi_trace: mahimahi_trace for BwReplayCell
macro_rules! bwreplay_q_args_into_config {
    ($q_type:ident, $q_args:expr, $trace_file:expr) => {
        paste!(
            match serde_json::from_str::<[<$q_type QueueConfig>]> (&$q_args.unwrap_or("{}".to_string())) {
                Ok(queue_config) => CellBuildConfig::BwReplay(BwReplayCellBuildConfig::$q_type(
                    if $q_args.is_none() {
                        BwReplayQueueConfig::new($trace_file, None, None)
                    } else {
                        BwReplayQueueConfig::new($trace_file, queue_config, None)
                    }
                )),
                Err(e) => {
                    tracing::error!("Failed to parse queue args {:?}: {}", $q_args, e);
                    return Err(rattan_core::error::Error::ConfigError(format!("Failed to parse queue args {:?}: {}", $q_args, e)));
                }
            }
        )
    };
}

impl ChannelArgs {
    pub fn build_rattan_config(self) -> rattan_core::error::Result<RattanConfig<StdPacket>> {
        let mut cells_config = HashMap::<String, CellBuildConfig<StdPacket>>::new();
        let mut links_config = HashMap::<String, String>::new();
        let mut uplink_count = 0;
        let mut downlink_count = 0;

        if let Some(bandwidth) = self.uplink_bandwidth {
            let cell_config = match self.uplink_queue {
                Some(QueueType::Infinite) | None => {
                    CellBuildConfig::Bw(BwCellBuildConfig::Infinite(BwCellConfig::new(
                        bandwidth,
                        InfiniteQueueConfig::new(),
                        None,
                    )))
                }
                Some(QueueType::DropTail) => {
                    bw_q_args_into_config!(DropTail, self.uplink_queue_args.clone(), bandwidth)
                }
                Some(QueueType::DropHead) => {
                    bw_q_args_into_config!(DropHead, self.uplink_queue_args.clone(), bandwidth)
                }
                Some(QueueType::CoDel) => {
                    bw_q_args_into_config!(CoDel, self.uplink_queue_args.clone(), bandwidth)
                }
            };
            uplink_count += 1;
            cells_config.insert(format!("up_{uplink_count}"), cell_config);
        } else if let Some(ref trace_file) = self.uplink_trace {
            let cell_config = match self.uplink_queue {
                Some(QueueType::Infinite) | None => {
                    CellBuildConfig::BwReplay(BwReplayCellBuildConfig::Infinite(
                        BwReplayQueueConfig::new(trace_file, InfiniteQueueConfig::new(), None),
                    ))
                }
                Some(QueueType::DropTail) => {
                    bwreplay_q_args_into_config!(
                        DropTail,
                        self.uplink_queue_args.clone(),
                        trace_file
                    )
                }
                Some(QueueType::DropHead) => {
                    bwreplay_q_args_into_config!(
                        DropHead,
                        self.uplink_queue_args.clone(),
                        trace_file
                    )
                }
                Some(QueueType::CoDel) => {
                    bwreplay_q_args_into_config!(CoDel, self.uplink_queue_args.clone(), trace_file)
                }
            };
            uplink_count += 1;
            cells_config.insert(format!("up_{uplink_count}"), cell_config);
        }

        if let Some(bandwidth) = self.downlink_bandwidth {
            let cell_config = match self.downlink_queue {
                Some(QueueType::Infinite) | None => {
                    CellBuildConfig::Bw(BwCellBuildConfig::Infinite(BwCellConfig::new(
                        bandwidth,
                        InfiniteQueueConfig::new(),
                        None,
                    )))
                }
                Some(QueueType::DropTail) => {
                    bw_q_args_into_config!(DropTail, self.downlink_queue_args.clone(), bandwidth)
                }
                Some(QueueType::DropHead) => {
                    bw_q_args_into_config!(DropHead, self.downlink_queue_args.clone(), bandwidth)
                }
                Some(QueueType::CoDel) => {
                    bw_q_args_into_config!(CoDel, self.downlink_queue_args.clone(), bandwidth)
                }
            };
            downlink_count += 1;
            cells_config.insert(format!("down_{downlink_count}"), cell_config);
        } else if let Some(trace_file) = self.downlink_trace {
            let cell_config = match self.downlink_queue {
                Some(QueueType::Infinite) | None => {
                    CellBuildConfig::BwReplay(BwReplayCellBuildConfig::Infinite(
                        BwReplayQueueConfig::new(trace_file, InfiniteQueueConfig::new(), None),
                    ))
                }
                Some(QueueType::DropTail) => {
                    bwreplay_q_args_into_config!(
                        DropTail,
                        self.downlink_queue_args.clone(),
                        trace_file
                    )
                }
                Some(QueueType::DropHead) => {
                    bwreplay_q_args_into_config!(
                        DropHead,
                        self.downlink_queue_args.clone(),
                        trace_file
                    )
                }
                Some(QueueType::CoDel) => {
                    bwreplay_q_args_into_config!(
                        CoDel,
                        self.downlink_queue_args.clone(),
                        trace_file
                    )
                }
            };
            downlink_count += 1;
            cells_config.insert(format!("down_{downlink_count}"), cell_config);
        }

        if let Some(delay) = self.uplink_delay {
            let cell_config = CellBuildConfig::Delay(DelayCellBuildConfig::new(delay));
            uplink_count += 1;
            cells_config.insert(format!("up_{uplink_count}"), cell_config);
        }

        if let Some(delay) = self.downlink_delay {
            let cell_config = CellBuildConfig::Delay(DelayCellBuildConfig::new(delay));
            downlink_count += 1;
            cells_config.insert(format!("down_{downlink_count}"), cell_config);
        }

        if let Some(loss) = self.uplink_loss {
            let cell_config = CellBuildConfig::Loss(LossCellBuildConfig::new([loss]));
            uplink_count += 1;
            cells_config.insert(format!("up_{uplink_count}"), cell_config);
        }

        if let Some(loss) = self.downlink_loss {
            let cell_config = CellBuildConfig::Loss(LossCellBuildConfig::new([loss]));
            downlink_count += 1;
            cells_config.insert(format!("down_{downlink_count}"), cell_config);
        }

        for i in 1..uplink_count {
            links_config.insert(format!("up_{i}"), format!("up_{}", i + 1));
        }
        for i in 1..downlink_count {
            links_config.insert(format!("down_{i}"), format!("down_{}", i + 1));
        }

        if uplink_count > 0 {
            links_config.insert("left".to_string(), "up_1".to_string());
            links_config.insert(format!("up_{uplink_count}"), "right".to_string());
        } else {
            links_config.insert("left".to_string(), "right".to_string());
        }
        if downlink_count > 0 {
            links_config.insert("right".to_string(), "down_1".to_string());
            links_config.insert(format!("down_{downlink_count}"), "left".to_string());
        } else {
            links_config.insert("right".to_string(), "left".to_string());
        }

        Ok(RattanConfig::<StdPacket> {
            env: StdNetEnvConfig {
                mode: StdNetEnvMode::Compatible,
                client_cores: vec![1],
                server_cores: vec![3],
                ..Default::default()
            },
            cells: cells_config,
            links: links_config,
            resource: RattanResourceConfig {
                memory: None,
                cpu: Some(vec![2]),
            },
            ..Default::default()
        })
    }
}
