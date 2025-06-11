use crate::{
    cells::{
        per_packet::delay::{DelayPerPacketCell, DelayPerPacketCellConfig},
        Packet,
    },
    core::CellFactory,
    error::Error,
};

use figment::{
    providers::{Format as _, Json, Toml},
    Figment,
};
use netem_trace::model::DelayPerPacketTraceConfig;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Clone, Debug)]
pub enum DelayPerPacketCellBuildConfig {
    FromFile(std::path::PathBuf),
    FromConfig(DelayPerPacketCellConfig),
}

impl DelayPerPacketCellBuildConfig {
    fn load(self) -> Result<DelayPerPacketCellConfig, Error> {
        match self {
            Self::FromConfig(config) => Ok(config),
            Self::FromFile(path) => {
                if let Some(ext) = path.extension() {
                    if ext == "json" {
                        let trace: Box<dyn DelayPerPacketTraceConfig> = Figment::new()
                            .merge(Json::file(path))
                            .extract()
                            .map_err(|e| Error::ConfigError(e.to_string()))?;
                        return Ok(DelayPerPacketCellConfig { delay: trace });
                    } else if ext == "toml" {
                        let trace: Box<dyn DelayPerPacketTraceConfig> = Figment::new()
                            .merge(Toml::file(path))
                            .extract()
                            .map_err(|e| Error::ConfigError(e.to_string()))?;
                        return Ok(DelayPerPacketCellConfig { delay: trace });
                    }
                }
                Err(Error::ConfigError(format!(
                    "Unknown trace file format: {:?}",
                    path
                )))
            }
        }
    }

    pub fn into_factory<P: Packet>(self) -> impl CellFactory<DelayPerPacketCell<P>> {
        move |handle| {
            let _guard = handle.enter();
            DelayPerPacketCell::new(self.load()?.delay.into_model())
        }
    }
}
