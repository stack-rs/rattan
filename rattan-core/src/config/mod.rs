use std::{collections::HashMap, env, path::PathBuf};

use rattan_env::env::RattanEnvConfig;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::cells::Packet;
#[cfg(feature = "http")]
use crate::control::http::HttpConfig;
use crate::radix::PacketLogMode;

mod bandwidth;
mod delay;
mod loss;
mod per_packet;
mod router;
mod shadow;
mod spy;
mod token_bucket;

pub use bandwidth::*;
pub use delay::*;
pub use loss::*;
pub use per_packet::*;
pub use router::*;
pub use shadow::*;
pub use spy::*;
pub use token_bucket::*;

/// Configuration for the whole Rattan system.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(bound = ""))]
#[derive(Clone, Debug)]
pub struct RattanConfig<P: Packet, EC: RattanEnvConfig> {
    #[cfg_attr(feature = "serde", serde(default))]
    pub env: EC,
    #[cfg(feature = "http")]
    #[cfg_attr(feature = "http", serde(default))]
    pub http: HttpConfig,
    #[cfg_attr(feature = "serde", serde(default))]
    pub cells: HashMap<String, CellBuildConfig<P>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub links: HashMap<String, String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub resource: RattanResourceConfig,
    #[cfg_attr(feature = "serde", serde(default))]
    pub general: RattanGeneralConfig,
}

impl<P: Packet, E: RattanEnvConfig> Default for RattanConfig<P, E> {
    fn default() -> Self {
        Self {
            env: E::default(),
            #[cfg(feature = "http")]
            http: HttpConfig::default(),
            cells: HashMap::new(),
            links: HashMap::new(),
            resource: RattanResourceConfig::new(),
            general: RattanGeneralConfig::new(),
        }
    }
}

#[cfg_attr(
    feature = "serde",
    serde_with::skip_serializing_none,
    derive(Serialize, Deserialize),
    serde(bound = "")
)]
#[derive(Clone, Debug, Default)]
pub struct RattanResourceConfig {
    #[cfg_attr(feature = "serde", serde(default))]
    pub memory: Option<usize>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub cpu: Option<Vec<u32>>,
}

impl RattanResourceConfig {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn working_threads(&self) -> usize {
        // If env `RATTAN_WORKING_THREADS` is set, use it; otherwise use cpu cores
        if let Some(threads) = env::var("RATTAN_WORKING_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            return threads;
        }
        if let Some(cpu) = &self.cpu {
            cpu.len()
        } else {
            4
        }
    }

    pub fn get_cpu(&self) -> Option<Vec<u32>> {
        // If env `RATTAN_CPU` is set, use it; otherwise use the cpu cores from the config
        if let Ok(cpu_list) = env::var("RATTAN_CPU") {
            // split by comma and parse each core
            let cpu: Vec<u32> = cpu_list.split(',').filter_map(|s| s.parse().ok()).collect();
            return Some(cpu);
        }
        self.cpu.clone()
    }
}

#[cfg_attr(
    feature = "serde",
    serde_with::skip_serializing_none,
    derive(Serialize, Deserialize),
    serde(bound = "")
)]
#[derive(Clone, Debug, Default)]
pub struct RattanGeneralConfig {
    #[cfg_attr(feature = "serde", serde(default))]
    pub packet_log: Option<PathBuf>,
    pub packet_log_mode: Option<PacketLogMode>,
}

impl RattanGeneralConfig {
    pub fn new() -> Self {
        Default::default()
    }
}

#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(bound = "", tag = "type")
)]
#[derive(Clone, Debug)]
pub enum CellBuildConfig<P: Packet> {
    Bw(BwCellBuildConfig<P>),
    BwReplay(BwReplayCellBuildConfig<P>),
    Delay(DelayCellBuildConfig),
    DelayReplay(DelayReplayCellBuildConfig),
    Loss(LossCellBuildConfig),
    LossReplay(LossReplayCellBuildConfig),
    Shadow(ShadowCellBuildConfig),
    Spy(SpyCellBuildConfig),
    DelayPerPacket(DelayPerPacketCellBuildConfig),
    Router(RouterCellBuildConfig),
    TokenBucket(TokenBucketCellBuildConfig),
    Custom,
}
