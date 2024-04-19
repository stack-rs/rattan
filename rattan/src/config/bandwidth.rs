use std::io::BufRead;

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
    Infinite(BwReplayCLIConfig<P, queue::InfiniteQueue<P>>),
    DropTail(BwReplayCLIConfig<P, queue::DropTailQueue<P>>),
    DropHead(BwReplayCLIConfig<P, queue::DropHeadQueue<P>>),
    CoDel(BwReplayCLIConfig<P, queue::CoDelQueue<P>>),
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(bound = ""))]
#[derive(Debug)]
pub struct BwReplayCLIConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub mahimahi_trace: Option<String>,
    pub trace_config_str: Option<String>,
    pub queue_config: Option<Q::Config>,
    pub bw_type: Option<BwType>,
}

impl<P, Q> Clone for BwReplayCLIConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
    Q::Config: Clone,
{
    fn clone(&self) -> Self {
        Self {
            mahimahi_trace: self.mahimahi_trace.clone(),
            trace_config_str: self.trace_config_str.clone(),
            queue_config: self.queue_config.clone(),
            bw_type: self.bw_type,
        }
    }
}

impl<P, Q> BwReplayCLIConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    pub fn new<
        T: Into<Option<String>>,
        U: Into<Option<String>>,
        V: Into<Option<Q::Config>>,
        W: Into<Option<BwType>>,
    >(
        mahimahi_trace: T,
        trace_config_str: U,
        queue_config: V,
        bw_type: W,
    ) -> Self {
        Self {
            mahimahi_trace: mahimahi_trace.into(),
            trace_config_str: trace_config_str.into(),
            queue_config: queue_config.into(),
            bw_type: bw_type.into(),
        }
    }
}

fn mahimahi_file_to_pattern(filename: &str) -> Result<Vec<u64>, Error> {
    let trace_file = std::fs::File::open(filename).map_err(|e| {
        error!("Failed to open trace file {}: {}", &filename, e);
        Error::IoError(e)
    })?;
    let trace_pattern = std::io::BufReader::new(trace_file)
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    error!("Failed to read line {} in {}: {}", i, &filename, e);
                    return Err(Error::IoError(e));
                }
            };
            let line = line.trim();
            line.parse::<u64>().map_err(|e| {
                error!("Failed to parse line {} in {}: {}", i, &filename, e);
                Error::ConfigError(format!("Failed to parse line {}: {}", i, e))
            })
        })
        .collect();
    debug!("Trace pattern in file {}: {:?}", &filename, trace_pattern);
    trace_pattern
}

impl<P, Q> BwReplayCLIConfig<P, Q>
where
    P: Packet,
    Q: PacketQueue<P>,
{
    fn get_trace(&self) -> Result<Box<dyn BwTrace>, Error> {
        if self.mahimahi_trace.is_some() && self.trace_config_str.is_some() {
            return Err(Error::ConfigError(
                "Only one of mahimahi_trace or trace_config_str can be specified".to_string(),
            ));
        }
        if let Some(mahimahi_trace) = &self.mahimahi_trace {
            let trace_pattern = mahimahi_file_to_pattern(mahimahi_trace)?;
            let trace = netem_trace::load_mahimahi_trace(trace_pattern, None).map_err(|e| {
                error!("Failed to load trace file {}: {}", mahimahi_trace, e);
                Error::ConfigError(format!(
                    "Failed to load trace file {}: {}",
                    mahimahi_trace, e
                ))
            })?;
            Ok(Box::new(trace.build()) as Box<dyn BwTrace>)
        } else if let Some(trace_config_str) = &self.trace_config_str {
            let trace_config: Box<dyn BwTraceConfig> = serde_json::from_str(trace_config_str)?;
            Ok(trace_config.into_model())
        } else {
            Err(Error::ConfigError(
                "Either mahimahi_trace or trace_config_str must be specified".to_string(),
            ))
        }
    }
}

macro_rules! impl_bw_replay_device_into_factory {
    ($($queue:ident),*) => {
        $(
            impl<P: Packet> BwReplayCLIConfig<P, queue::$queue<P>> {
                pub fn into_factory(
                    self,
                ) -> impl DeviceFactory<bandwidth::BwReplayDevice<P, queue::$queue<P>>> {
                    move |handle| {
                        let _guard = handle.enter();
                        let trace = self.get_trace().map_err(|e| {
                            error!("Failed to get trace: {}", e);
                            e
                        })?;
                        let queue = queue::$queue::<P>::new(self.queue_config.unwrap_or_default());
                        BwReplayDevice::new(trace, queue, self.bw_type.unwrap_or_default())
                    }
                }
            }
        )*
    };
}

impl_bw_replay_device_into_factory!(InfiniteQueue, DropTailQueue, DropHeadQueue, CoDelQueue);
