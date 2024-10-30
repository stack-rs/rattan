use std::{collections::HashMap, path::PathBuf};

use crate::{cells::Packet, env::StdNetEnvConfig};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "http")]
use crate::control::http::HttpConfig;

mod bandwidth;
mod delay;
mod loss;
mod router;
mod shadow;

pub use bandwidth::*;
pub use delay::*;
pub use loss::*;
pub use router::*;
pub use shadow::*;

/// Configuration for the whole Rattan system.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(bound = ""))]
#[derive(Clone, Debug)]
pub struct RattanConfig<P: Packet> {
    #[cfg_attr(feature = "serde", serde(default))]
    pub env: StdNetEnvConfig,
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

impl<P: Packet> Default for RattanConfig<P> {
    fn default() -> Self {
        Self {
            env: StdNetEnvConfig::default(),
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
    Router(RouterCellBuildConfig),
    Custom,
}
