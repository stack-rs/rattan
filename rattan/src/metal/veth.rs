use crate::error::{Error, MacParseError, VethError};
use crate::metal::netns::{NetNs, NetNsGuard};
use nix::net::if_::if_nametoindex;
use rtnetlink::{new_connection, Handle};
use std::net::IpAddr;
use std::os::fd::AsRawFd;
use std::process::Command;
use std::sync::{Arc, Mutex, Weak};
use tokio::runtime::Runtime;

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
            eprintln!("Failed to enter netns: {}", e);
            return;
        }

        let output = Command::new("ip")
            .arg("link")
            .arg("del")
            .arg(&self.left.lock().unwrap().name)
            .output();

        if let Err(e) = output {
            eprintln!("Failed to delete veth pair: {} (you may need to delete it manually with 'sudo ip link del {}')", e, &self.left.lock().unwrap().name);
        } else {
            let output = output.unwrap();
            if !output.status.success() {
                eprintln!("Failed to delete veth pair: {} (you may need to delete it manually with 'sudo ip link del {}')", String::from_utf8_lossy(&output.stderr), &self.left.lock().unwrap().name);
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

/// Generate a runtime and a rtnetlink handle for one time use.
fn rtnetlink_once() -> (Runtime, Handle) {
    let rt: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();
    let (conn, rtnl_handle, _) = new_connection().unwrap();
    rt.spawn(conn);
    (rt, rtnl_handle)
}

/// Used to asynchronously delete a veth pair.
fn delete_veth(left: Arc<Mutex<VethDevice>>) {
    let ns_guard = NetNsGuard::new(left.lock().unwrap().namespace.clone());
    if let Err(e) = ns_guard {
        eprintln!("Failed to enter netns: {}", e);
        return;
    }

    let (rt, rtnl_handle) = rtnetlink_once();
    match rt.block_on(rtnl_handle.link().del(left.lock().unwrap().index).execute()) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Failed to delete veth pair: {} (you may need to delete it manually with 'sudo ip link del {}')", e, &left.lock().unwrap().name);
        }
    }
}

pub struct VethPair {
    pub left: Arc<Mutex<VethDevice>>,
    pub right: Arc<Mutex<VethDevice>>,
}

impl VethPair {
    pub fn new<S: AsRef<str>>(name: S, peer_name: S) -> Result<VethPair, VethError> {
        let (rt, rtnl_handle) = rtnetlink_once();
        match rt.block_on(
            rtnl_handle
                .link()
                .add()
                .veth(
                    String::from(name.as_ref()),
                    String::from(peer_name.as_ref()),
                )
                .execute(),
        ) {
            Ok(_) => {
                let pair = VethPair {
                    left: Arc::new(Mutex::new(VethDevice {
                        name: name.as_ref().to_string(),
                        index: if_nametoindex(name.as_ref())?,
                        mac_addr: None,
                        ip_addr: None,
                        peer: None,
                        namespace: NetNs::current()?,
                    })),
                    right: Arc::new(Mutex::new(VethDevice {
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
            Err(e) => Err(VethError::CreateVethPairError(e.to_string())),
        }
    }
}

impl Drop for VethPair {
    // FIXME: This is a hack to work because we can't use tokio block_on here.
    fn drop(&mut self) {
        let left = self.left.clone();
        let handle = std::thread::spawn(move || {
            delete_veth(left);
        });
        handle.join().unwrap();
    }
}

pub struct VethDevice {
    pub name: String,
    pub index: u32,
    pub mac_addr: Option<MacAddr>,
    pub ip_addr: Option<(IpAddr, u8)>,
    pub peer: Option<Weak<Mutex<VethDevice>>>,
    namespace: std::sync::Arc<NetNs>,
}

impl VethDevice {
    pub fn peer(&self) -> Arc<Mutex<VethDevice>> {
        self.peer.as_ref().unwrap().upgrade().unwrap()
    }

    pub fn up(&mut self) -> Result<&mut Self, Error> {
        let _ns_guard = NetNsGuard::new(self.namespace.clone())?;
        let (rt, rtnl_handle) = rtnetlink_once();
        match rt.block_on(rtnl_handle.link().set(self.index).up().execute()) {
            Ok(_) => Ok(self),
            Err(e) => Err(VethError::SetError(e.to_string()).into()),
        }
    }

    pub fn down(&mut self) -> Result<&mut Self, Error> {
        let _ns_guard = NetNsGuard::new(self.namespace.clone())?;
        let (rt, rtnl_handle) = rtnetlink_once();
        match rt.block_on(rtnl_handle.link().set(self.index).down().execute()) {
            Ok(_) => Ok(self),
            Err(e) => Err(VethError::SetError(e.to_string()).into()),
        }
    }

    pub fn set_ns(&mut self, netns: std::sync::Arc<NetNs>) -> Result<&mut Self, VethError> {
        let _ns_guard = NetNsGuard::new(self.namespace.clone())?;
        let (rt, rtnl_handle) = rtnetlink_once();
        match rt.block_on(
            rtnl_handle
                .link()
                .set(self.index)
                .setns_by_fd(netns.as_raw_fd())
                .execute(),
        ) {
            Ok(_) => {
                self.namespace = netns;
                Ok(self)
            }
            Err(e) => Err(VethError::SetError(e.to_string())),
        }
    }

    pub fn set_l2_addr(&mut self, mac_addr: MacAddr) -> Result<&mut Self, Error> {
        let _ns_guard = NetNsGuard::new(self.namespace.clone())?;
        let (rt, rtnl_handle) = rtnetlink_once();
        match rt.block_on(
            rtnl_handle
                .link()
                .set(self.index)
                .address(Vec::from(mac_addr.bytes))
                .execute(),
        ) {
            Ok(_) => {
                self.mac_addr = Some(mac_addr);
                Ok(self)
            }
            Err(e) => Err(VethError::SetError(e.to_string()).into()),
        }
    }

    pub fn set_l3_addr(&mut self, ip_addr: IpAddr, prefix: u8) -> Result<&mut Self, Error> {
        let _ns_guard = NetNsGuard::new(self.namespace.clone())?;
        let (rt, rtnl_handle) = rtnetlink_once();
        match rt.block_on(
            rtnl_handle
                .address()
                .add(self.index, ip_addr, prefix)
                .execute(),
        ) {
            Ok(_) => {
                self.ip_addr = Some((ip_addr, prefix));
                Ok(self)
            }
            Err(e) => Err(VethError::SetError(e.to_string()).into()),
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
