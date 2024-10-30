use crate::{
    core::CellFactory,
    cells::{shadow, Packet},
};

pub type ShadowCellBuildConfig = shadow::ShadowCellConfig;

impl ShadowCellBuildConfig {
    pub fn into_factory<P: Packet>(self) -> impl CellFactory<shadow::ShadowCell<P>> {
        move |handle| {
            let _guard = handle.enter();
            shadow::ShadowCell::new()
        }
    }
}
