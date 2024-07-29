use std::sync::Arc;

use crate::{
    core::DeviceFactory,
    devices::{
        router::{self, SimpleRoutingTable},
        Ingress, Packet,
    },
};

pub type RouterDeviceBuildConfig = router::RouterDeviceConfig;

impl RouterDeviceBuildConfig {
    pub fn into_factory<P: Packet>(
        self,
        receivers: Vec<Arc<dyn Ingress<P>>>,
    ) -> impl DeviceFactory<router::RouterDevice<P, SimpleRoutingTable>> {
        move |handle| {
            let _guard = handle.enter();
            router::RouterDevice::new(receivers, self.routing_table)
        }
    }
}
