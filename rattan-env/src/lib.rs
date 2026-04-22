pub mod common;
pub mod env;
pub mod error;
pub mod interface;
pub mod ioctl;
pub mod netns;
pub mod route;
pub mod timer;
pub mod veth;

// Top-level re-exports for ergonomic API
pub use common::Packet;
pub use env::standard::{
    get_container_env, get_std_env, ContainerEnv, StdNetEnv, StdNetEnvConfig, StdNetEnvMode,
};
pub use error::{Error, MacParseError, NsError, TokioRuntimeError, VethError};
pub use interface::{InterfaceBuildArtifact, InterfaceDriver, InterfaceReceiver, InterfaceSender};
pub use netns::{DefaultEnv, Env, NetNs, NetNsGuard};
pub use route::{add_arp_entry_with_netns, add_route_with_netns, set_loopback_up_with_netns};
pub use veth::{
    set_rps_cores, IpVethCell, IpVethPair, MacAddr, VethCell, VethPair, VethPairBuilder,
};

pub use env::{RattanEnv, RattanEnvConfig};
