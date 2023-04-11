use crate::error::{Error, MacParseError, VethError};
use crate::netns::{get_current_netns, NetNs};
use nix::net::if_::if_nametoindex;
use std::net::IpAddr;
use std::process::Command;

pub struct VethPair {
    pub left: VethDevice,
    pub right: VethDevice,
}

impl VethPair {
    pub fn new<S: AsRef<str>>(name: S, peer_name: S) -> Result<VethPair, VethError> {
        let output = Command::new("ip")
            .arg("link")
            .arg("add")
            .arg(name.as_ref())
            .arg("type")
            .arg("veth")
            .arg("peer")
            .arg("name")
            .arg(peer_name.as_ref())
            .output()?;
        if !output.status.success() {
            Err(VethError::CreateVethPairError(
                String::from_utf8(output.stderr).unwrap(),
            ))
        } else {
            Ok(VethPair {
                left: VethDevice {
                    name: name.as_ref().to_string(),
                    index: if_nametoindex(name.as_ref())?,
                    mac_addr: None,
                    ip_addr: None,
                    ns_name: None,
                },
                right: VethDevice {
                    name: peer_name.as_ref().to_string(),
                    index: if_nametoindex(peer_name.as_ref())?,
                    mac_addr: None,
                    ip_addr: None,
                    ns_name: None,
                },
            })
        }
    }
}

impl Drop for VethPair {
    fn drop(&mut self) {
        let cur_netns = if let Some(ref ns) = self.left.ns_name {
            let cur_netns = get_current_netns().unwrap();
            let netns = NetNs::get(ns).unwrap();
            netns.enter().unwrap();
            Some(cur_netns)
        } else {
            None
        };
        let output = Command::new("ip")
            .arg("link")
            .arg("del")
            .arg(&self.left.name)
            .output();
        if let Some(netns) = cur_netns {
            netns.enter().unwrap();
        }
        if let Err(e) = output {
            eprintln!("Failed to delete veth pair: {} (you may need to delete it manually with 'sudo ip link del {}')", e, &self.left.name);
        } else {
            let output = output.unwrap();
            if !output.status.success() {
                eprintln!("Failed to delete veth pair: {} (you may need to delete it manually with 'sudo ip link del {}')", String::from_utf8_lossy(&output.stderr), &self.left.name);
            }
        }
    }
}

pub struct VethDevice {
    pub name: String,
    pub index: u32,
    pub mac_addr: Option<MacAddr>,
    pub ip_addr: Option<(IpAddr, u8)>,
    ns_name: Option<String>,
}

impl VethDevice {
    pub fn up(&self) -> Result<(), Error> {
        let cur_netns = if let Some(ref ns) = self.ns_name {
            let cur_netns = get_current_netns()?;
            let netns = NetNs::get(ns)?;
            netns.enter()?;
            Some(cur_netns)
        } else {
            None
        };
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
        if let Some(netns) = cur_netns {
            netns.enter()?;
        }
        if !output.status.success() {
            Err(VethError::SetError(String::from_utf8(output.stderr).unwrap()).into())
        } else {
            Ok(())
        }
    }

    pub fn down(&self) -> Result<(), Error> {
        let cur_netns = if let Some(ref ns) = self.ns_name {
            let cur_netns = get_current_netns()?;
            let netns = NetNs::get(ns)?;
            netns.enter()?;
            Some(cur_netns)
        } else {
            None
        };
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
        if let Some(netns) = cur_netns {
            netns.enter()?;
        }
        if !output.status.success() {
            Err(VethError::SetError(String::from_utf8(output.stderr).unwrap()).into())
        } else {
            Ok(())
        }
    }

    pub fn set_ns<S: Into<String>>(&mut self, ns_name: S) -> Result<(), VethError> {
        if let Some(ref ns) = self.ns_name {
            return Err(VethError::AlreadyInNamespace(ns.clone()));
        }
        let ns_name = ns_name.into();
        let output = Command::new("ip")
            .arg("link")
            .arg("set")
            .arg("dev")
            .arg(&self.name)
            .arg("netns")
            .arg(&ns_name)
            .output()?;
        if !output.status.success() {
            Err(VethError::SetError(
                String::from_utf8(output.stderr).unwrap(),
            ))
        } else {
            self.ns_name = Some(ns_name);
            Ok(())
        }
    }

    pub fn set_l2_addr(&mut self, mac_addr: MacAddr) -> Result<(), Error> {
        let cur_netns = if let Some(ref ns) = self.ns_name {
            let cur_netns = get_current_netns()?;
            let netns = NetNs::get(ns)?;
            netns.enter()?;
            Some(cur_netns)
        } else {
            None
        };
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
        if let Some(netns) = cur_netns {
            netns.enter()?;
        }
        if !output.status.success() {
            Err(VethError::SetError(String::from_utf8(output.stderr).unwrap()).into())
        } else {
            self.mac_addr = Some(mac_addr);
            Ok(())
        }
    }

    pub fn set_l3_addr(&mut self, ip_addr: IpAddr, prefix: u8) -> Result<(), Error> {
        let cur_netns = if let Some(ref ns) = self.ns_name {
            let cur_netns = get_current_netns()?;
            let netns = NetNs::get(ns)?;
            netns.enter()?;
            Some(cur_netns)
        } else {
            None
        };
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
        if let Some(netns) = cur_netns {
            netns.enter()?;
        }
        if !output.status.success() {
            Err(VethError::SetError(String::from_utf8(output.stderr).unwrap()).into())
        } else {
            self.ip_addr = Some((ip_addr, prefix));
            Ok(())
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
