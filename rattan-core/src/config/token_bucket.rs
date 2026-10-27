use crate::{
    cells::{Packet, token_bucket},
    core::CellFactory,
};

pub type TokenBucketCellBuildConfig = token_bucket::TokenBucketCellConfig;

impl TokenBucketCellBuildConfig {
    pub fn into_factory<P: Packet>(self) -> impl CellFactory<token_bucket::TokenBucketCell<P>> {
        move |handle| {
            let _guard = handle.enter();
            token_bucket::TokenBucketCell::new(
                self.limit,
                self.rate,
                self.burst,
                self.peak_rate,
                self.min_burst,
            )
        }
    }
}
