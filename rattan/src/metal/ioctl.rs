use libc::{__c_anonymous_ifr_ifru, c_char, c_uint, ifreq};
use nix::{
    errno::Errno,
    ioctl_readwrite_bad,
    sys::socket::{socket, AddressFamily, SockFlag, SockType},
};

use crate::error::VethError;

struct EthtoolValue {
    pub cmd: u32,
    pub data: u32,
}

struct OffFlagDef {
    #[allow(dead_code)]
    pub name: &'static str,

    #[allow(dead_code)]
    pub get_cmd: c_uint,

    #[allow(dead_code)]
    pub set_cmd: c_uint,
}

const SIOCETHTOOL: u32 = 0x8946;

const OFF_FLAG_DEF_SIZE: usize = 7;

const ETHTOOL_GRXCSUM: u32 = 0x00000014; /* Get RX hw csum enable (ethtool_value) */
const ETHTOOL_SRXCSUM: u32 = 0x00000015; /* Set RX hw csum enable (ethtool_value) */
const ETHTOOL_GTXCSUM: u32 = 0x00000016; /* Get TX hw csum enable (ethtool_value) */
const ETHTOOL_STXCSUM: u32 = 0x00000017; /* Set TX hw csum enable (ethtool_value) */
const ETHTOOL_GSG: u32 = 0x00000018; /* Get scatter-gather enable (ethtool_value) */
const ETHTOOL_SSG: u32 = 0x00000019; /* Set scatter-gather enable (ethtool_value). */
const ETHTOOL_GTSO: u32 = 0x0000001e; /* Get TSO enable (ethtool_value) */
const ETHTOOL_STSO: u32 = 0x0000001f; /* Set TSO enable (ethtool_value) */
const ETHTOOL_GUFO: u32 = 0x00000021; /* Get UFO enable (ethtool_value) */
const ETHTOOL_SUFO: u32 = 0x00000022; /* Set UFO enable (ethtool_value) */
const ETHTOOL_GGSO: u32 = 0x00000023; /* Get GSO enable (ethtool_value) */
const ETHTOOL_SGSO: u32 = 0x00000024; /* Set GSO enable (ethtool_value) */
const ETHTOOL_GGRO: u32 = 0x0000002b; /* Get GRO enable (ethtool_value) */
const ETHTOOL_SGRO: u32 = 0x0000002c; /* Set GRO enable (ethtool_value) */

const OFF_FLAG_DEF: [OffFlagDef; OFF_FLAG_DEF_SIZE] = [
    OffFlagDef {
        name: "rx-checksum",
        get_cmd: ETHTOOL_GRXCSUM,
        set_cmd: ETHTOOL_SRXCSUM,
    },
    OffFlagDef {
        name: "tx-checksum-*",
        get_cmd: ETHTOOL_GTXCSUM,
        set_cmd: ETHTOOL_STXCSUM,
    },
    OffFlagDef {
        name: "tx-scatter-gather*",
        get_cmd: ETHTOOL_GSG,
        set_cmd: ETHTOOL_SSG,
    },
    OffFlagDef {
        name: "tx-tcp*-segmentation",
        get_cmd: ETHTOOL_GTSO,
        set_cmd: ETHTOOL_STSO,
    },
    OffFlagDef {
        name: "tx-udp-fragmentation",
        get_cmd: ETHTOOL_GUFO,
        set_cmd: ETHTOOL_SUFO,
    },
    OffFlagDef {
        name: "tx-generic-segmentation",
        get_cmd: ETHTOOL_GGSO,
        set_cmd: ETHTOOL_SGSO,
    },
    OffFlagDef {
        name: "rx-gro",
        get_cmd: ETHTOOL_GGRO,
        set_cmd: ETHTOOL_SGRO,
    },
];

ioctl_readwrite_bad!(ethtool_ioctl, SIOCETHTOOL, ifreq);

pub fn get_feature_flag(name: &str) -> Result<[u32; OFF_FLAG_DEF_SIZE], VethError> {
    let fd = socket(
        AddressFamily::Inet,
        SockType::Datagram,
        SockFlag::empty(),
        None,
    )
    .unwrap();

    let mut ifr = ifreq {
        ifr_name: [0; libc::IF_NAMESIZE],
        ifr_ifru: __c_anonymous_ifr_ifru {
            ifru_data: std::ptr::null_mut(),
        },
    };

    ifr.ifr_name[..name.len()].copy_from_slice(unsafe {
        std::slice::from_raw_parts(name.as_ptr() as *const i8, name.len())
    });

    let mut flags = [0; OFF_FLAG_DEF_SIZE];
    for (i, off_flag_def) in OFF_FLAG_DEF.iter().enumerate() {
        let mut eval = EthtoolValue { cmd: 0, data: 0 };
        eval.cmd = off_flag_def.get_cmd;
        eval.data = 0;
        ifr.ifr_ifru.ifru_data = &mut eval as *mut EthtoolValue as *mut c_char;

        let res = unsafe { ethtool_ioctl(fd, &mut ifr) };
        match res {
            Ok(_) => {
                flags[i] = eval.data;
            }
            Err(Errno::EOPNOTSUPP) => {}
            Err(e) => {
                return Err(VethError::SetError(e.desc().to_string()));
            }
        }
    }
    Ok(flags)
}

pub fn disable_checksum_offload(name: &str) -> Result<(), VethError> {
    let fd = socket(
        AddressFamily::Inet,
        SockType::Datagram,
        SockFlag::empty(),
        None,
    )
    .unwrap();

    let mut ifr = ifreq {
        ifr_name: [0; libc::IF_NAMESIZE],
        ifr_ifru: __c_anonymous_ifr_ifru {
            ifru_data: std::ptr::null_mut(),
        },
    };

    ifr.ifr_name[..name.len()].copy_from_slice(unsafe {
        std::slice::from_raw_parts(name.as_ptr() as *const i8, name.len())
    });

    for off_flag_def in OFF_FLAG_DEF.iter() {
        let mut eval = EthtoolValue { cmd: 0, data: 0 };
        eval.cmd = off_flag_def.set_cmd;
        eval.data = 0;
        ifr.ifr_ifru.ifru_data = &mut eval as *mut EthtoolValue as *mut c_char;

        let res = unsafe { ethtool_ioctl(fd, &mut ifr) };
        match res {
            Ok(_) => {}
            Err(Errno::EOPNOTSUPP) => {}
            Err(e) => {
                return Err(VethError::SetError(e.desc().to_string()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{disable_checksum_offload, get_feature_flag, OFF_FLAG_DEF_SIZE};

    struct VethTest();
    impl VethTest {
        fn new() -> Self {
            let output = Command::new("ip")
                .arg("link")
                .arg("add")
                .arg("veth0-test")
                .arg("type")
                .arg("veth")
                .arg("peer")
                .arg("name")
                .arg("veth1-test")
                .output()
                .unwrap();
            if output.status.success() {
                println!("veth0-test and veth1-test created");
            } else {
                panic!(
                    "veth0-test and veth1-test creation failed: {}",
                    String::from_utf8(output.stderr).unwrap()
                );
            }
            VethTest()
        }
    }

    impl Drop for VethTest {
        fn drop(&mut self) {
            let output = Command::new("ip")
                .arg("link")
                .arg("del")
                .arg("veth0-test")
                .output()
                .unwrap();
            if output.status.success() {
                println!("veth0-test and veth1-test deleted");
            } else {
                panic!(
                    "veth0-test and veth1-test deletion failed: {}",
                    String::from_utf8(output.stderr).unwrap()
                );
            }
        }
    }

    #[test]
    fn test_ioctl() {
        let _veth = VethTest::new();
        disable_checksum_offload("veth0-test").unwrap();
        let flags = get_feature_flag("veth0-test").unwrap();
        assert_eq!(flags, [0; OFF_FLAG_DEF_SIZE]);
    }
}
