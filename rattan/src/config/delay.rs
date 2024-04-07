use crate::{
    core::DeviceFactory,
    devices::{delay, Packet},
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
