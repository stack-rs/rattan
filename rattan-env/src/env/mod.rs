use std::fs::create_dir_all;
use std::net::IpAddr;
use std::sync::Arc;

use futures::TryStreamExt;
use rtnetlink::packet_route::address::AddressAttribute;
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

    fn default_with_mode(mode: <Self::BuildOutput as RattanEnv<Self::Driver>>::Mode) -> Self;
    fn default_compatible() -> Self;
    fn default_isolated() -> Self;
}

const IP_LOCK_DIR: &str = "/tmp/rattan/ip_lock";

struct IpAddrLock {
    file_dir: String,
}

impl IpAddrLock {
    /// Lock an IP address by creating a file with the same name as the it
    fn new(ip: IpAddr) -> Result<Option<Self>, std::io::Error> {
        // for ipv6, ':' may be illegal as a filename
        let ip_str = format!("{ip}").replace(':', "_");
        let file_dir = format!("{IP_LOCK_DIR}/{ip_str}");

        create_dir_all(IP_LOCK_DIR)?;
        match std::fs::File::create_new(&file_dir) {
            // Lock successfully
            Ok(_) => Ok(Some(IpAddrLock { file_dir })),
            // Lock file already exists
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            // Other error
            Err(e) => Err(e),
        }
    }
}

impl Drop for IpAddrLock {
    /// Unlock by removing the lock file
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file_dir);
    }
}

fn get_addresses_in_use() -> Result<Vec<IpAddr>, crate::error::Error> {
    tracing::debug!("Get addresses in use");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            tracing::error!("Failed to build rtnetlink runtime");
            crate::error::Error::TokioRuntimeError(e.into())
        })?;
    let _guard = rt.enter();
    let (conn, rtnl_handle, _) = rtnetlink::new_connection()?;
    rt.spawn(conn);

    let mut addresses = vec![];
    rt.block_on(async {
        let mut links = rtnl_handle.address().get().execute();
        while let Ok(Some(address_msg)) = links.try_next().await {
            for address_attr in address_msg.attributes {
                if let AddressAttribute::Address(address) = address_attr {
                    tracing::trace!(?address, ?address_msg.header.prefix_len, "Get address");
                    addresses.push(address);
                }
            }
        }
    });
    tracing::debug!(?addresses, "Addresses in use");
    Ok(addresses)
}
