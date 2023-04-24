use std::sync::Arc;

use crate::devices::ControlInterface;

#[cfg(feature = "http")]
pub mod http;

pub trait ControlEndpoint {
    fn register_device(&mut self, index: usize, control_interface: Arc<impl ControlInterface>);
}
