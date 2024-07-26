use figment::{
    providers::{Format, Json, Toml},
    Figment,
};
use netem_trace::{model::LossTraceConfig, LossTrace};
use rand::{rngs::StdRng, SeedableRng};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
    core::DeviceFactory,
    devices::{loss, Packet},
    error::Error,
};

pub type LossDeviceBuildConfig = loss::LossDeviceConfig;

impl LossDeviceBuildConfig {
    pub fn into_factory<P: Packet>(self) -> impl DeviceFactory<loss::LossDevice<P, StdRng>> {
        move |handle| {
            let _guard = handle.enter();
            // let rng = StdRng::from_entropy();
            let rng = StdRng::seed_from_u64(42); // XXX: fixed seed for reproducibility
            loss::LossDevice::new(self.pattern, rng)
        }
    }
}

#[cfg_attr(
    feature = "serde",
    serde_with::skip_serializing_none,
    derive(Serialize, Deserialize)
)]
#[derive(Debug, Clone)]
pub struct LossReplayDeviceBuildConfig {
    pub trace: String,
    pub seed: Option<u64>,
}

impl LossReplayDeviceBuildConfig {
    fn get_trace(&self) -> Result<Box<dyn LossTrace>, Error> {
        let file_path = std::path::Path::new(&self.trace);
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
            "Unknown trace file format: {:?}",
            file_path
        )))
    }

    pub fn into_factory<P: Packet>(self) -> impl DeviceFactory<loss::LossReplayDevice<P, StdRng>> {
        move |handle| {
            let _guard = handle.enter();
            let trace = self.get_trace()?;
            let rng = StdRng::seed_from_u64(self.seed.unwrap_or(42));
            loss::LossReplayDevice::new(trace, rng)
        }
    }
}
