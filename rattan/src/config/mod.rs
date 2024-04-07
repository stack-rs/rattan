use std::collections::HashMap;

use crate::{devices::Packet, env::StdNetEnvConfig};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "http")]
use crate::control::http::HttpConfig;

mod bandwidth;
mod delay;
mod loss;

pub use bandwidth::*;
pub use delay::*;
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
    #[cfg_attr(feature = "serde", serde(flatten))]
    pub core: RattanCoreConfig<P>,
}

impl<P: Packet> Default for RattanConfig<P> {
    fn default() -> Self {
        Self {
            env: StdNetEnvConfig::default(),
            #[cfg(feature = "http")]
            http: HttpConfig::default(),
            core: RattanCoreConfig::default(),
        }
    }
}

/// Configuration for the Rattan core.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(bound = ""))]
#[derive(Clone, Debug)]
pub struct RattanCoreConfig<P: Packet> {
    #[cfg_attr(feature = "serde", serde(default))]
    pub devices: HashMap<String, DeviceBuildConfig<P>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub links: HashMap<String, String>,
}

impl<P: Packet> Default for RattanCoreConfig<P> {
    fn default() -> Self {
        Self {
            devices: HashMap::new(),
            links: HashMap::new(),
        }
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
    Loss(LossDeviceBuildConfig),
    Custom,
}
