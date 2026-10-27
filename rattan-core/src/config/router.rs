use std::sync::Arc;

use crate::{
    cells::{
        Ingress, Packet,
        router::{self, SimpleRoutingTable},
    },
    core::CellFactory,
};

pub type RouterCellBuildConfig = router::RouterCellConfig;

impl RouterCellBuildConfig {
    pub fn into_factory<P: Packet>(
        self,
        receivers: Vec<Arc<dyn Ingress<P>>>,
    ) -> impl CellFactory<router::RouterCell<P, SimpleRoutingTable>> {
        move |handle| {
            let _guard = handle.enter();
            router::RouterCell::new(receivers, self.routing_table)
        }
    }
}
