use figment::{
    providers::{Format, Json, Toml},
    Figment,
};

use netem_trace::{model::DelayTraceConfig, DelayTrace};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
    core::DeviceFactory,
    devices::{delay, Packet},
    error::Error,
};

pub type DelayDeviceBuildConfig = delay::DelayDeviceConfig;

impl DelayDeviceBuildConfig {
    pub fn into_factory<P: Packet>(self) -> impl DeviceFactory<delay::DelayDevice<P>> {
        move |handle| {
            let _guard = handle.enter();
            delay::DelayDevice::new(self.delay)
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone)]
pub struct DelayReplayDeviceBuildConfig {
    pub trace: String,
}

impl DelayReplayDeviceBuildConfig {
    fn get_trace(&self) -> Result<Box<dyn DelayTrace>, Error> {
        let file_path = std::path::Path::new(&self.trace);
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
            "Unknown trace file format: {:?}",
            file_path
        )))
    }
    pub fn into_factory<P: Packet>(self) -> impl DeviceFactory<delay::DelayReplayDevice<P>> {
        move |handle| {
            let _guard = handle.enter();
            let trace = self.get_trace()?;
            delay::DelayReplayDevice::new(trace)
        }
    }
}
