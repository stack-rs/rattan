use std::sync::Arc;

mod bandwidth;
mod delay;
mod loss;
lazy_static::lazy_static! {
    pub static ref STD_ENV_LOCK: Arc<parking_lot::Mutex<()>> = Arc::new(parking_lot::Mutex::new(()));
}
