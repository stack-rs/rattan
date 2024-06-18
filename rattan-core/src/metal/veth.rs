use crate::error::{Error, MacParseError, VethError};
use crate::metal::netns::{NetNs, NetNsGuard};
use nix::net::if_::if_nametoindex;
use once_cell::sync::OnceCell;
use std::net::IpAddr;
use std::os::fd::AsRawFd;
use std::process::Command;
use std::sync::{Arc, Mutex, Weak};
// use tempfile::TempDir;
use tracing::{debug, error, info, instrument, span, Level};

use super::ioctl::disable_checksum_offload;

pub struct IpVethPair {
    pub left: Arc<Mutex<IpVethDevice>>,
    pub right: Arc<Mutex<IpVethDevice>>,
}

impl IpVethPair {
    pub fn new<S: AsRef<str>>(name: S, peer_name: S) -> Result<IpVethPair, VethError> {
        let handle = Command::new("ip")
            .arg("link")
            .arg("add")
            .arg(name.as_ref())
            .arg("type")
            .arg("veth")
            .arg("peer")
            .arg("name")
            .arg(peer_name.as_ref())
            .spawn()?;
        let output = handle.wait_with_output()?;
        if !output.status.success() {
            Err(VethError::CreateVethPairError(
                String::from_utf8(output.stderr).unwrap(),
            ))
        } else {
            let pair = IpVethPair {
                left: Arc::new(Mutex::new(IpVethDevice {
                    name: name.as_ref().to_string(),
                    index: if_nametoindex(name.as_ref())?,
                    mac_addr: None,
                    ip_addr: None,
                    peer: None,
                    namespace: NetNs::current()?,
                })),
                right: Arc::new(Mutex::new(IpVethDevice {
                    name: peer_name.as_ref().to_string(),
                    index: if_nametoindex(peer_name.as_ref())?,
                    mac_addr: None,
                    ip_addr: None,
                    peer: None,
                    namespace: NetNs::current()?,
                })),
            };

            pair.left
                .lock()
                .unwrap()
                .peer
                .replace(Arc::downgrade(&pair.right));
            pair.right
                .lock()
                .unwrap()
                .peer
                .replace(Arc::downgrade(&pair.left));

            Ok(pair)
        }
    }
}

impl Drop for IpVethPair {
    fn drop(&mut self) {
        let ns_guard = NetNsGuard::new(self.left.lock().unwrap().namespace.clone());
        if let Err(e) = ns_guard {
            error!("Failed to enter netns: {}", e);
            return;
        }

        let output = Command::new("ip")
            .arg("link")
            .arg("del")
            .arg(&self.left.lock().unwrap().name)
            .output();

        if let Err(e) = output {
            error!("Failed to delete veth pair: {} (you may need to delete it manually with 'sudo ip link del {}')", e, &self.left.lock().unwrap().name);
        } else {
            let output = output.unwrap();
            if !output.status.success() {
                error!("Failed to delete veth pair: {} (you may need to delete it manually with 'sudo ip link del {}')", String::from_utf8_lossy(&output.stderr), &self.left.lock().unwrap().name);
            }
        }
    }
}

pub struct IpVethDevice {
    pub name: String,
    pub index: u32,
    pub mac_addr: Option<MacAddr>,
    pub ip_addr: Option<(IpAddr, u8)>,
    pub peer: Option<Weak<Mutex<IpVethDevice>>>,
    namespace: std::sync::Arc<NetNs>,
}

impl IpVethDevice {
    pub fn peer(&self) -> Arc<Mutex<IpVethDevice>> {
        self.peer.as_ref().unwrap().upgrade().unwrap()
    }

    pub fn up(&mut self) -> Result<&mut Self, Error> {
        let _ns_guard = NetNsGuard::new(self.namespace.clone())?;
        let output = Command::new("ip")
            .arg("link")
            .arg("set")
            .arg("dev")
            .arg(&self.name)
            .arg("up")
            .output()
            .map_err(|e| {
                let err: VethError = e.into();
                err
            })?;
        if !output.status.success() {
            Err(VethError::SetError(String::from_utf8(output.stderr).unwrap()).into())
        } else {
            Ok(self)
        }
    }

    pub fn down(&mut self) -> Result<&mut Self, Error> {
        let _ns_guard = NetNsGuard::new(self.namespace.clone())?;
        let output = Command::new("ip")
            .arg("link")
            .arg("set")
            .arg("dev")
            .arg(&self.name)
            .arg("down")
            .output()
            .map_err(|e| {
                let err: VethError = e.into();
                err
            })?;
        if !output.status.success() {
            Err(VethError::SetError(String::from_utf8(output.stderr).unwrap()).into())
        } else {
            Ok(self)
        }
    }

    pub fn set_ns(&mut self, netns: std::sync::Arc<NetNs>) -> Result<&mut Self, VethError> {
        if netns == self.namespace {
            return Ok(self);
        }

        // XXX(minhuw): ad-hoc solution here, maybe ip netns accept path, i.e., /var/run/netns/<netns>?
        // or rtnetlink may use fd instead of so we can use netns.file directly.
        let ns_name = netns
            .as_ref()
            .path()
            .strip_prefix("/var/run/netns/")
            .unwrap();

        let output = Command::new("ip")
            .arg("link")
            .arg("set")
            .arg("dev")
            .arg(&self.name)
            .arg("netns")
            .arg(ns_name)
            .output()?;
        if !output.status.success() {
            Err(VethError::SetError(
                String::from_utf8(output.stderr).unwrap(),
            ))
        } else {
            self.namespace = netns;
            Ok(self)
        }
    }

    pub fn set_l2_addr(&mut self, mac_addr: MacAddr) -> Result<&mut Self, Error> {
        let _ns_guard = NetNsGuard::new(self.namespace.clone())?;
        let output = Command::new("ip")
            .arg("link")
            .arg("set")
            .arg("dev")
            .arg(&self.name)
            .arg("address")
            .arg(mac_addr.to_string())
            .output()
            .map_err(|e| {
                let err: VethError = e.into();
                err
            })?;
        if !output.status.success() {
            Err(VethError::SetError(String::from_utf8(output.stderr).unwrap()).into())
        } else {
            self.mac_addr = Some(mac_addr);
            Ok(self)
        }
    }

    pub fn set_l3_addr(&mut self, ip_addr: IpAddr, prefix: u8) -> Result<&mut Self, Error> {
        let _ns_guard = NetNsGuard::new(self.namespace.clone())?;
        let output = Command::new("ip")
            .arg("address")
            .arg("add")
            .arg(format!("{}/{}", ip_addr, prefix))
            .arg("dev")
            .arg(&self.name)
            .output()
            .map_err(|e| {
                let err: VethError = e.into();
                err
            })?;
        if !output.status.success() {
            Err(VethError::SetError(String::from_utf8(output.stderr).unwrap()).into())
        } else {
            self.ip_addr = Some((ip_addr, prefix));
            Ok(self)
        }
    }

    pub fn disable_checksum_offload(&mut self) -> Result<&mut Self, Error> {
        let _ns_guard = NetNsGuard::new(self.namespace.clone())?;
        let output = Command::new("ethtool")
            .args(["-K", &self.name, "tx", "off", "rx", "off"])
            .output()?;
        if !output.status.success() {
            Err(VethError::SetError(String::from_utf8(output.stderr).unwrap()).into())
        } else {
            Ok(self)
        }
    }
}

#[derive(Debug)]
pub struct VethPair {
    pub left: Arc<VethDevice>,
    pub right: Arc<VethDevice>,
}

#[derive(Debug, Clone)]
pub struct VethDevice {
    pub name: String,
    pub index: u32,
    pub mac_addr: MacAddr,
    pub ip_addr: (IpAddr, u8),
    pub peer: OnceCell<Weak<VethDevice>>,
    pub namespace: Arc<NetNs>,
}

#[derive(Debug)]
pub struct VethPairBuilder {
    name: Option<(String, String)>,
    mac_addr: Option<(MacAddr, MacAddr)>,
    ip_addr: Option<((IpAddr, u8), (IpAddr, u8))>,
    namespace: (Option<Arc<NetNs>>, Option<Arc<NetNs>>),
}

impl Default for VethPairBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl VethPairBuilder {
    pub fn new() -> Self {
        Self {
            name: None,
            mac_addr: None,
            ip_addr: None,
            namespace: (None, None),
        }
    }

    pub fn name(mut self, left: String, right: String) -> Self {
        self.name = Some((left, right));
        self
    }

    pub fn mac_addr(mut self, left: MacAddr, right: MacAddr) -> Self {
        self.mac_addr = Some((left, right));
        self
    }

    pub fn ip_addr(mut self, left: (IpAddr, u8), right: (IpAddr, u8)) -> Self {
        self.ip_addr = Some((left, right));
        self
    }

    pub fn namespace(mut self, left: Option<Arc<NetNs>>, right: Option<Arc<NetNs>>) -> Self {
        self.namespace = (left, right);
        self
    }

    async fn build_impl(
        self,
        tokio_handle: &tokio::runtime::Handle,
    ) -> Result<Arc<VethPair>, VethError> {
        let name = match self.name {
            Some(name) => name,
            None => {
                return Err(VethError::CreateVethPairError(
                    "Veth pair name is not specified.".to_string(),
                ));
            }
        };
        let mac_addr = match self.mac_addr {
            Some(mac_addr) => mac_addr,
            None => {
                return Err(VethError::CreateVethPairError(
                    "Veth pair MAC address is not specified.".to_string(),
                ));
            }
        };
        let ip_addr = match self.ip_addr {
            Some(ip_addr) => ip_addr,
            None => {
                return Err(VethError::CreateVethPairError(
                    "Veth pair IP address is not specified.".to_string(),
                ));
            }
        };

        let (conn, rtnl_handle, _) = rtnetlink::new_connection()?;
        tokio_handle.spawn(conn);
        rtnl_handle
            .link()
            .add()
            .veth(name.clone().0, name.clone().1)
            .execute()
            .await
            .map_err(|e| VethError::CreateVethPairError(e.to_string()))?;

        let pair = VethPair {
            left: Arc::new(VethDevice {
                name: name.clone().0,
                index: if_nametoindex(name.clone().0.as_str())?,
                mac_addr: mac_addr.0,
                ip_addr: ip_addr.0,
                peer: OnceCell::new(),
                namespace: self.namespace.0.unwrap_or(NetNs::current()?),
            }),
            right: Arc::new(VethDevice {
                name: name.clone().1,
                index: if_nametoindex(name.clone().1.as_str())?,
                mac_addr: mac_addr.1,
                ip_addr: ip_addr.1,
                peer: OnceCell::new(),
                namespace: self.namespace.1.unwrap_or(NetNs::current()?),
            }),
        };
        pair.left.peer.set(Arc::downgrade(&pair.right)).unwrap();
        pair.right.peer.set(Arc::downgrade(&pair.left)).unwrap();

        for device in [pair.left.clone(), pair.right.clone()] {
            // Set namespace
            rtnl_handle
                .link()
                .set(device.index)
                .setns_by_fd(device.namespace.as_raw_fd())
                .execute()
                .await
                .map_err(|e| VethError::SetError(e.to_string()))?;

            // Enter namespace
            let _ns_guard: NetNsGuard = NetNsGuard::new(device.namespace.clone())?;
            std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime spawn
            let (conn, rtnl_handle, _) = rtnetlink::new_connection()?;
            tokio_handle.spawn(conn);

            // Set mac address
            rtnl_handle
                .link()
                .set(device.index)
                .address(Vec::from(device.mac_addr.bytes))
                .execute()
                .await
                .map_err(|e| VethError::SetError(e.to_string()))?;

            // Set ip address
            rtnl_handle
                .address()
                .add(device.index, device.ip_addr.0, device.ip_addr.1)
                .execute()
                .await
                .map_err(|e| VethError::SetError(e.to_string()))?;

            // Set up
            rtnl_handle
                .link()
                .set(device.index)
                .up()
                .execute()
                .await
                .map_err(|e| VethError::SetError(e.to_string()))?;

            // Disable checksum offload
            disable_checksum_offload(device.name.as_str())?;
        }

        info!(
            "Veth pair created: {:>15} <--> {:<15}",
            &pair.left.name, &pair.right.name
        );
        info!(
            "                   {:>15} <--> {:<15}",
            &pair.left.ip_addr.0, &pair.right.ip_addr.0
        );
        Ok(Arc::new(pair))
    }

    #[instrument(name = "VethPairBuilder", skip_all, ret(level = Level::TRACE), err)]
    pub fn build(self) -> Result<Arc<VethPair>, VethError> {
        debug!(?self, "Building veth pair...");
        let build_thread_span = span!(Level::DEBUG, "build_thread").or_current();
        let build_thread = std::thread::spawn(|| {
            let _entered = build_thread_span.entered();
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    error!("Failed to build rtnetlink runtime");
                    VethError::TokioRuntimeError(e.into())
                })?;
            rt.block_on(self.build_impl(rt.handle()))
        });
        build_thread.join().unwrap()
    }
}

impl Drop for VethPair {
    fn drop(&mut self) {
        let left = self.left.clone();
        let left_name = left.name.clone();
        let right = self.right.clone();
        let build_thread_span = span!(Level::DEBUG, "build_thread").or_current();
        let build_thread = std::thread::spawn(move || -> Result<(), Error> {
            let _entered = build_thread_span.entered();
            let _ns_guard = NetNsGuard::new(left.namespace.clone()).inspect_err(|e| {
                error!("Failed to enter netns: {}", e);
            })?;
            std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    error!("Failed to build rtnetlink runtime");
                    Error::TokioRuntimeError(e.into())
                })?;
            let _guard = rt.enter();
            let (conn, rtnl_handle, _) = rtnetlink::new_connection()?;
            rt.spawn(conn);
            rt.block_on(rtnl_handle.link().del(left.index).execute())
                .map(|_| {
                    debug!(
                        "Veth pair deleted: {:>15} <--> {:<15}",
                        &left.name, &right.name
                    );
                })
                .map_err(|e| {
                    error!("Failed to delete veth pair: {}", e);
                    Error::MetalError(e.into())
                })
        });
        if let Err(e) = build_thread.join().unwrap() {
            error!("Failed to delete veth pair: {} (you may need to delete it manually with 'sudo ip link del {}')", e, left_name);
        };
    }
}

impl VethDevice {
    pub fn peer(&self) -> Arc<VethDevice> {
        self.peer.get().unwrap().upgrade().unwrap()
    }
}

/// Contains the individual bytes of the MAC address.
#[derive(Debug, Clone, Copy, PartialEq, Default, Eq, PartialOrd, Ord, Hash)]
pub struct MacAddr {
    bytes: [u8; 6],
}

impl MacAddr {
    /// Creates a new `MacAddr` struct from the given bytes.
    pub fn new(bytes: [u8; 6]) -> MacAddr {
        MacAddr { bytes }
    }

    /// Returns the array of MAC address bytes.
    pub fn bytes(self) -> [u8; 6] {
        self.bytes
    }
}

impl From<[u8; 6]> for MacAddr {
    fn from(v: [u8; 6]) -> Self {
        MacAddr::new(v)
    }
}

impl std::str::FromStr for MacAddr {
    type Err = MacParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let mut array = [0u8; 6];

        let mut nth = 0;
        for byte in input.split(|c| c == ':' || c == '-') {
            if nth == 6 {
                return Err(MacParseError::InvalidLength);
            }

            array[nth] = u8::from_str_radix(byte, 16).map_err(|_| MacParseError::InvalidDigit)?;

            nth += 1;
        }

        if nth != 6 {
            return Err(MacParseError::InvalidLength);
        }

        Ok(MacAddr::new(array))
    }
}

impl std::convert::TryFrom<&'_ str> for MacAddr {
    type Error = MacParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl std::convert::TryFrom<std::borrow::Cow<'_, str>> for MacAddr {
    type Error = MacParseError;

    fn try_from(value: std::borrow::Cow<'_, str>) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl std::fmt::Display for MacAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let _ = write!(
            f,
            "{:<02X}:{:<02X}:{:<02X}:{:<02X}:{:<02X}:{:<02X}",
            self.bytes[0],
            self.bytes[1],
            self.bytes[2],
            self.bytes[3],
            self.bytes[4],
            self.bytes[5]
        );

        Ok(())
    }
}

// fn remount_sys() -> Result<TempDir, VethError> {
//     let temp_dir = TempDir::with_prefix("ns_sys")?;

//     Command::new("mount")
//         .args([
//             "-t",
//             "sysfs",
//             "none",
//             temp_dir.path().as_os_str().to_str().unwrap(),
//         ])
//         .spawn()?
//         .wait()?;

//     Ok(temp_dir)
// }

pub fn set_rps_cores(_name: &str, _cores: &[usize]) {
    // TODO(minhu): wait for future refactor. For privilege reason, we just comment this.
    // let temp_dir = remount_sys().unwrap();
    // let sys_path = format!("{}/class/net/{}/queues", temp_dir.path().display(), name);

    // for entry in std::fs::read_dir(sys_path).unwrap() {
    //     let entry = entry.unwrap();
    //     let path = entry.path();
    //     if path.is_dir()
    //         && path
    //             .file_name()
    //             .unwrap()
    //             .to_str()
    //             .unwrap()
    //             .starts_with("rx-")
    //     {
    //         let file = path.join("rps_cpus");
    //         let bitmap = cores.iter().map(|c| 1u64 << c).fold(0, |acc, m| acc | m);

    //         println!("write {:x} to {}", bitmap, file.display());

    //         std::fs::write(file, format!("{bitmap:x}")).unwrap();
    //     }
    // }
}
