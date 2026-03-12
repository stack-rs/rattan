use std::time::Duration;

use figment::{
    providers::{Format, Json, Toml},
    Figment,
};
use netem_trace::{model::DelayTraceConfig, DelayTrace};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
    cells::{delay, Packet},
    core::CellFactory,
    error::Error,
    utils::replace_env_var_in_string,
};

pub type DelayCellBuildConfig = delay::DelayCellConfig;

impl DelayCellBuildConfig {
    pub fn into_factory<P: Packet>(self) -> impl CellFactory<delay::DelayCell<P>> {
        move |handle| {
            let _guard = handle.enter();
            delay::DelayCell::with_ingress_round_up(self.delay, self.ingress_round_up)
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone)]
pub struct DelayReplayCellBuildConfig {
    pub trace: String,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(
        feature = "serde",
        serde(with = "crate::utils::serde::duration::option")
    )]
    pub ingress_round_up: Option<Duration>,
}

impl DelayReplayCellBuildConfig {
    pub fn get_trace(&self) -> Result<Box<dyn DelayTrace>, Error> {
        let parsed_trace_path = replace_env_var_in_string(&self.trace);
        let file_path = std::path::Path::new(parsed_trace_path.as_ref());
        if let Some(ext) = file_path.extension() {
            if ext == "json" {
                let trace: Box<dyn DelayTraceConfig> = Figment::new()
                    .merge(Json::file(file_path))
                    .extract()
                    .map_err(|e| Error::ConfigError(e.to_string()))?;
                return Ok(trace.into_model());
            } else if ext == "toml" {
                let trace: Box<dyn DelayTraceConfig> = Figment::new()
                    .merge(Toml::file(file_path))
                    .extract()
                    .map_err(|e| Error::ConfigError(e.to_string()))?;
                return Ok(trace.into_model());
            }
        }
        Err(Error::ConfigError(format!(
            "Unknown trace file format: {}",
            file_path.display()
        )))
    }

    pub fn into_factory<P: Packet>(self) -> impl CellFactory<delay::DelayReplayCell<P>> {
        move |handle| {
            let _guard = handle.enter();
            let trace = self.get_trace()?;
            delay::DelayReplayCell::with_ingress_round_up(trace, self.ingress_round_up)
        }
    }
}
