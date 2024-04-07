use rand::{rngs::StdRng, SeedableRng};

use crate::{
    core::DeviceFactory,
    devices::{loss, Packet},
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
