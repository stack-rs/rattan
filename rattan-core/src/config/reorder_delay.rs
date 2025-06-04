use std::time::Duration;

use crate::{
    cells::{
        reorder_delay::{
            delay::{DelayGenerator, LogNormalLawDelayGenerator, NormalLawDelayGenerator},
            ReorderDelayCell, ReorderDelayCellConfig,
        },
        Packet,
    },
    core::CellFactory,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Clone, Debug)]
pub enum ReorderDelayCellBuildConfig {
    Constant(ReorderDelayCellConfig<Duration>),
    NormalLaw(ReorderDelayCellConfig<NormalLawDelayGenerator>),
    LogNormalLaw(ReorderDelayCellConfig<LogNormalLawDelayGenerator>),
}

impl<D: DelayGenerator> ReorderDelayCellConfig<D> {
    pub fn into_factory<P: Packet>(self) -> impl CellFactory<ReorderDelayCell<P, D>> {
        move |handle| {
            let _guard = handle.enter();
            ReorderDelayCell::new(self.delay)
        }
    }
}
