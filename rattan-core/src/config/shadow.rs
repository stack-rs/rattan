use crate::{
    core::DeviceFactory,
    devices::{shadow, Packet},
};

pub type ShadowDeviceBuildConfig = shadow::ShadowDeviceConfig;

impl ShadowDeviceBuildConfig {
    pub fn into_factory<P: Packet>(self) -> impl DeviceFactory<shadow::ShadowDevice<P>> {
        move |handle| {
            let _guard = handle.enter();
            shadow::ShadowDevice::new()
        }
    }
}
