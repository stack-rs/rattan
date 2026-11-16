use std::sync::Arc;

mod af_packet;
mod std_env;

// Environment
pub use std_env::{
    get_container_env, get_std_env, ContainerEnv, StdNetEnv, StdNetEnvConfig, StdNetEnvMode,
};
// IO Driver
pub use af_packet::{AfPacketDriver, AfPacketReceiver, AfPacketSender, StdPacket};

#[cfg(feature = "xdp")]
mod af_xdp;
#[cfg(feature = "xdp")]
pub use af_xdp::{XDPDriver, XDPPacket, XDPReceiver, XDPSender};
use tokio::runtime::Handle;

pub(crate) trait VethLikeDriver: crate::InterfaceDriver + Sized {
    fn bind_cell(
        cell: Arc<crate::VethCell>,
        handle: &Handle,
    ) -> Result<Vec<Self>, crate::error::MetalError>;
}
