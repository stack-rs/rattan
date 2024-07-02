use std::collections::HashMap;

use crate::{devices::Packet, env::StdNetEnvConfig};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "http")]
use crate::control::http::HttpConfig;

mod bandwidth;
mod delay;
mod duplicate;
mod loss;

pub use bandwidth::*;
pub use delay::*;
pub use duplicate::*;
pub use loss::*;

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
    pub devices: HashMap<String, DeviceBuildConfig<P>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub links: HashMap<String, String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub resource: RattanResourceConfig,
}

impl<P: Packet> Default for RattanConfig<P> {
    fn default() -> Self {
        Self {
            env: StdNetEnvConfig::default(),
            #[cfg(feature = "http")]
            http: HttpConfig::default(),
            devices: HashMap::new(),
            links: HashMap::new(),
            resource: RattanResourceConfig::new(),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(bound = ""))]
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
    derive(Serialize, Deserialize),
    serde(bound = "", tag = "type")
)]
#[derive(Clone, Debug)]
pub enum DeviceBuildConfig<P: Packet> {
    Bw(BwDeviceBuildConfig<P>),
    BwReplay(BwReplayDeviceBuildConfig<P>),
    Delay(DelayDeviceBuildConfig),
    DelayReplay(DelayReplayDeviceBuildConfig),
    Loss(LossDeviceBuildConfig),
    LossReplay(LossReplayDeviceBuildConfig),
    Duplicate(DuplicateDeviceBuildConfig),
    DuplicateReplay(DuplicateReplayDeviceBuildConfig),
    Custom,
}
