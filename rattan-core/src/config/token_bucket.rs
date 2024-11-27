use crate::{
    cells::{bandwidth::queue::DropTailQueue, token_bucket::{self, TokenBucket}, Packet},
    core::CellFactory,
};

pub type TokenBucketCellBuildConfig = token_bucket::TokenBucketCellConfig;

impl TokenBucketCellBuildConfig {
    pub fn into_factory<P: Packet>(self) -> impl CellFactory<token_bucket::TokenBucketCell<P>> {
        move |handle| {
            let _guard = handle.enter();
            token_bucket::TokenBucketCell::new(
                TokenBucket::from(self.token_bucket.unwrap_or_default()), 
                TokenBucket::from(self.peak_token_bucket.unwrap_or_default()),
                DropTailQueue::from(self.queue_config.unwrap_or_default())
            )
        }
    }
}