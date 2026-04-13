use std::net::IpAddr;
use std::sync::Arc;

use tokio::runtime::Handle;

use crate::InterfaceDriver;

use crate::{error::MetalError, InterfaceBuildArtifact, NetNs};

#[cfg(feature = "serde")]
/// If serde is enabled, this trait provides a default implementation of `Serialize` and `Deserialize` for any type.
/// Otherwise, it provides a no-op implementation.
pub trait SerdeBounds: serde::Serialize + for<'de> serde::Deserialize<'de> {}

#[cfg(feature = "serde")]
impl<T> SerdeBounds for T where T: serde::Serialize + for<'de> serde::Deserialize<'de> {}

#[cfg(not(feature = "serde"))]
pub trait SerdeBounds {}

#[cfg(not(feature = "serde"))]
impl<T> SerdeBounds for T {}

#[cfg(feature = "rvnic")]
pub mod rvnic;
pub mod standard;

pub trait RattanEnv<D: InterfaceDriver> {
    type Mode: Clone;
    /// A unique identifier for the Rattan environment.
    fn get_rattan_id(&self) -> &str;
    /// Returns the network namespace that the working threads of rattan will run in.
    /// If this returns `None`, the working threads will run in the current network namespace.
    fn get_rattan_ns(&self) -> Option<Arc<NetNs>>;

    /// Returns the network namespace that the left side of the user process will run in.
    fn get_left_ns(&self) -> Arc<NetNs>;
    /// Returns the network namespace that the right side of the user process will run in.
    fn get_right_ns(&self) -> Arc<NetNs>;

    fn get_mode(&self) -> Self::Mode;
    /// IP of i-th veth pair of `ns-right`
    ///
    /// 0 is for external connection
    fn right_ip(&self, i: usize) -> IpAddr;
    /// IP of i-th veth pair of `ns-left`
    ///
    /// 0 is for external connection
    fn left_ip(&self, i: usize) -> IpAddr;
    /// IP list of veth pairs of `ns-right`
    fn right_ip_list(&self) -> Vec<(usize, IpAddr)>;
    /// IP list of veth pairs of `ns-left`
    fn left_ip_list(&self) -> Vec<(usize, IpAddr)>;
    /// Maximum ID of veth pairs of `ns-left`
    fn left_max_id(&self) -> usize;
    /// Maximum ID of veth pairs of `ns-right`
    fn right_max_id(&self) -> usize;

    fn build_interfaces(
        &mut self,
        runtime: &Handle,
    ) -> Result<Vec<InterfaceBuildArtifact<D>>, MetalError>;
}

pub trait RattanEnvConfig: SerdeBounds + Default {
    type Driver: InterfaceDriver;
    type BuildOutput: RattanEnv<Self::Driver>;
    type BuildError: Into<crate::error::Error>;

    fn build(
        &self,
        // args: Self::BuildArgs,
    ) -> Result<Self::BuildOutput, Self::BuildError>;

    /// Returns the CPU for the working threads of the async runtime to run on.
    fn get_running_cpu(&self) -> Vec<usize> {
        vec![1]
    }

    fn default_with_mode(mode: <Self::BuildOutput as RattanEnv<Self::Driver>>::Mode) -> Self;
    fn default_compatible() -> Self;
    fn default_isolated() -> Self;
}
