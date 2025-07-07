use figment::{
    providers::{Format, Json, Toml},
    Figment,
};
use netem_trace::{model::LossTraceConfig, LossTrace};
use rand::{rngs::StdRng, SeedableRng};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
    cells::{loss, Packet},
    core::CellFactory,
    error::Error,
    utils::replace_env_var_in_string,
};

pub type LossCellBuildConfig = loss::LossCellConfig;

impl LossCellBuildConfig {
    pub fn into_factory<P: Packet>(self) -> impl CellFactory<loss::LossCell<P, StdRng>> {
        move |handle| {
            let _guard = handle.enter();
            // let rng = StdRng::from_entropy();
            let rng = StdRng::seed_from_u64(42); // XXX: fixed seed for reproducibility
            loss::LossCell::new(self.pattern, rng)
        }
    }
}

#[cfg_attr(
    feature = "serde",
    serde_with::skip_serializing_none,
    derive(Serialize, Deserialize)
)]
#[derive(Debug, Clone)]
pub struct LossReplayCellBuildConfig {
    pub trace: String,
    pub seed: Option<u64>,
}

impl LossReplayCellBuildConfig {
    fn get_trace(&self) -> Result<Box<dyn LossTrace>, Error> {
        let parsed_trace_path = replace_env_var_in_string(&self.trace);
        let file_path = std::path::Path::new(parsed_trace_path.as_ref());
        if let Some(ext) = file_path.extension() {
            if ext == "json" {
                let trace: Box<dyn LossTraceConfig> = Figment::new()
                    .merge(Json::file(file_path))
                    .extract()
                    .map_err(|e| Error::ConfigError(e.to_string()))?;
                return Ok(trace.into_model());
            } else if ext == "toml" {
                let trace: Box<dyn LossTraceConfig> = Figment::new()
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

    pub fn into_factory<P: Packet>(self) -> impl CellFactory<loss::LossReplayCell<P, StdRng>> {
        move |handle| {
            let _guard = handle.enter();
            let trace = self.get_trace()?;
            let rng = StdRng::seed_from_u64(self.seed.unwrap_or(42));
            loss::LossReplayCell::new(trace, rng)
        }
    }
}
