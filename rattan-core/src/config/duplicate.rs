use figment::{
    providers::{Format, Json, Toml},
    Figment,
};
use netem_trace::{model::DuplicateTraceConfig, DuplicatePattern, DuplicateTrace};
use rand::{rngs::StdRng, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::{
    core::DeviceFactory,
    devices::{duplicate, Packet},
    error::Error,
};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone)]
pub struct DuplicateDeviceBuildConfig {
    pub pattern: DuplicatePattern,
    pub seed: Option<u64>,
}

impl DuplicateDeviceBuildConfig {
    pub fn into_factory<P: Packet>(
        self,
    ) -> impl DeviceFactory<duplicate::DuplicateDevice<P, StdRng>> {
        move |handle| {
            let _guard = handle.enter();
            let rng = StdRng::seed_from_u64(self.seed.unwrap_or(42));
            duplicate::DuplicateDevice::new(self.pattern, rng)
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone)]
pub struct DuplicateReplayDeviceBuildConfig {
    pub trace: String,
    pub seed: Option<u64>,
}

impl DuplicateReplayDeviceBuildConfig {
    fn get_trace(&self) -> Result<Box<dyn DuplicateTrace>, Error> {
        let file_path = std::path::Path::new(&self.trace);
        if let Some(ext) = file_path.extension() {
            if ext == "json" {
                let trace: Box<dyn DuplicateTraceConfig> = Figment::new()
                    .merge(Json::file(file_path))
                    .extract()
                    .map_err(|e| Error::ConfigError(e.to_string()))?;
                return Ok(trace.into_model());
            } else if ext == "toml" {
                let trace: Box<dyn DuplicateTraceConfig> = Figment::new()
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

    pub fn into_factory<P: Packet>(
        self,
    ) -> impl DeviceFactory<duplicate::DuplicateReplayDevice<P, StdRng>> {
        move |handle| {
            let _guard = handle.enter();
            let trace = self.get_trace()?;
            let rng = StdRng::seed_from_u64(self.seed.unwrap_or(42));
            duplicate::DuplicateReplayDevice::new(trace, rng)
        }
    }
}
