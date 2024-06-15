use std::{io::BufRead, path::Path};

use figment::{
    providers::{Format, Json, Toml},
    Figment,
};
use netem_trace::{model::BwTraceConfig, BwTrace};
use tracing::{debug, error};

use crate::{
    core::DeviceFactory,
    devices::{
        bandwidth::{
            self,
            queue::{self, PacketQueue},
            BwDevice, BwReplayDevice, BwType,
        },
        Packet,
    },
    error::Error,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(bound = "", tag = "queue")
)]
#[derive(Clone, Debug)]
pub enum BwDeviceBuildConfig<P: Packet> {
    Infinite(bandwidth::BwDeviceConfig<P, queue::InfiniteQueue<P>>),
    DropTail(bandwidth::BwDeviceConfig<P, queue::DropTailQueue<P>>),
    DropHead(bandwidth::BwDeviceConfig<P, queue::DropHeadQueue<P>>),
    CoDel(bandwidth::BwDeviceConfig<P, queue::CoDelQueue<P>>),
}

macro_rules! impl_bw_device_into_factory {
    ($($queue:ident),*) => {
        $(
            impl<P: Packet> bandwidth::BwDeviceConfig<P, queue::$queue<P>> {
                pub fn into_factory(
                    self,
                ) -> impl DeviceFactory<bandwidth::BwDevice<P, queue::$queue<P>>> {
                    move |handle| {
                        let _guard = handle.enter();
                        let queue = queue::$queue::<P>::new(self.queue_config.unwrap_or_default());
                        BwDevice::new(self.bandwidth, queue, self.bw_type.unwrap_or_default())
                    }
                }
            }
        )*
    };
}

impl_bw_device_into_factory!(InfiniteQueue, DropTailQueue, DropHeadQueue, CoDelQueue);

#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(bound = "", tag = "queue")
)]
#[derive(Clone, Debug)]
pub enum BwReplayDeviceBuildConfig<P: Packet> {
    Infinite(BwReplayQueueConfig<P, queue::InfiniteQueue<P>>),
    DropTail(BwReplayQueueConfig<P, queue::DropTailQueue<P>>),
    DropHead(BwReplayQueueConfig<P, queue::DropHeadQueue<P>>),
    CoDel(BwReplayQueueConfig<P, queue::CoDelQueue<P>>),
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(bound = ""))]
#[derive(Debug)]
pub struct BwReplayQueueConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub trace: String,
    pub queue_config: Option<Q::Config>,
    pub bw_type: Option<BwType>,
}

impl<P, Q> Clone for BwReplayQueueConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
    Q::Config: Clone,
{
    fn clone(&self) -> Self {
        Self {
            trace: self.trace.clone(),
            queue_config: self.queue_config.clone(),
            bw_type: self.bw_type,
        }
    }
}

impl<P, Q> BwReplayQueueConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new<T: Into<String>, V: Into<Option<Q::Config>>, W: Into<Option<BwType>>>(
        trace: T,
        queue_config: V,
        bw_type: W,
    ) -> Self {
        Self {
            trace: trace.into(),
            queue_config: queue_config.into(),
            bw_type: bw_type.into(),
        }
    }
}

fn mahimahi_file_to_pattern<P>(filename: P) -> Result<Vec<u64>, Error>
where
    P: AsRef<Path>,
{
    let trace_file = std::fs::File::open(filename.as_ref()).map_err(|e| {
        error!(
            "Failed to open trace file {}: {}",
            filename.as_ref().display(),
            e
        );
        Error::IoError(e)
    })?;
    let trace_pattern = std::io::BufReader::new(trace_file)
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    error!(
                        "Failed to read line {} in {}: {}",
                        i,
                        filename.as_ref().display(),
                        e
                    );
                    return Err(Error::IoError(e));
                }
            };
            let line = line.trim();
            line.parse::<u64>().map_err(|e| {
                error!(
                    "Failed to parse line {} in {}: {}",
                    i,
                    filename.as_ref().display(),
                    e
                );
                Error::ConfigError(format!("Failed to parse line {}: {}", i, e))
            })
        })
        .collect();
    debug!(
        "Trace pattern in file {}: {:?}",
        filename.as_ref().display(),
        trace_pattern
    );
    trace_pattern
}

impl<P, Q> BwReplayQueueConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    fn get_trace(&self) -> Result<Box<dyn BwTrace>, Error> {
        let file_path = std::path::Path::new(&self.trace);
        if let Some(ext) = file_path.extension() {
            if ext == "json" {
                let trace: Box<dyn BwTraceConfig> = Figment::new()
                    .merge(Json::file(file_path))
                    .extract()
                    .map_err(|e| Error::ConfigError(e.to_string()))?;
                return Ok(trace.into_model());
            } else if ext == "toml" {
                let trace: Box<dyn BwTraceConfig> = Figment::new()
                    .merge(Toml::file(file_path))
                    .extract()
                    .map_err(|e| Error::ConfigError(e.to_string()))?;
                return Ok(trace.into_model());
            } else {
                let trace_pattern = mahimahi_file_to_pattern(file_path)?;
                let trace = netem_trace::load_mahimahi_trace(trace_pattern, None).map_err(|e| {
                    Error::ConfigError(format!(
                        "Failed to load trace file {}: {}",
                        file_path.display(),
                        e
                    ))
                })?;
                return Ok(Box::new(trace.build()) as Box<dyn BwTrace>);
            }
        }
        Err(Error::ConfigError(format!(
            "Unknown trace file format: {:?}",
            file_path
        )))
    }
}

macro_rules! impl_bw_replay_device_into_factory {
    ($($queue:ident),*) => {
        $(
            impl<P: Packet> BwReplayQueueConfig<P, queue::$queue<P>> {
                pub fn into_factory(
                    self,
                ) -> impl DeviceFactory<bandwidth::BwReplayDevice<P, queue::$queue<P>>> {
                    move |handle| {
                        let _guard = handle.enter();
                        let trace = self.get_trace()?;
                        let queue = queue::$queue::<P>::new(self.queue_config.unwrap_or_default());
                        BwReplayDevice::new(trace, queue, self.bw_type.unwrap_or_default())
                    }
                }
            }
        )*
    };
}

impl_bw_replay_device_into_factory!(InfiniteQueue, DropTailQueue, DropHeadQueue, CoDelQueue);
