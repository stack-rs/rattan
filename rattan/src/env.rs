use crate::metal::veth::VethPairBuilder;
use crate::metal::{netns::NetNs, veth::VethPair};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

//   ns-client                          ns-rattan                         ns-server
// +-----------+    veth pair    +--------------------+    veth pair    +-----------+
// |    rc-left| <-------------> |rc-right [P] rs-left| <-------------> |rs-right   |
// |   .11.1/24|                 |.11.2/24    .12.2/24|                 |.12.1/24   |
// +-----------+                 +--------------------+                 +-----------+

lazy_static::lazy_static! {
    static ref STD_ENV_LOCK: Arc<parking_lot::Mutex<()>> = Arc::new(parking_lot::Mutex::new(()));
}

pub enum StdNetEnvMode {
    Compatible,
    Isolated,
    Container,
}

pub struct StdNetEnvConfig {
    pub mode: StdNetEnvMode,
}

impl Default for StdNetEnvConfig {
    fn default() -> Self {
        StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
        }
    }
}

pub struct StdNetEnv {
    pub left_ns: Arc<NetNs>,
    pub rattan_ns: Arc<NetNs>,
    pub right_ns: Arc<NetNs>,
    pub left_pair: Arc<VethPair>,
    pub right_pair: Arc<VethPair>,
}

pub fn get_std_env(config: StdNetEnvConfig) -> anyhow::Result<StdNetEnv> {
    let _guard = STD_ENV_LOCK.lock();
    let rand_string: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(6)
        .map(char::from)
        .collect();
    let client_netns_name = format!("ns-client-{}", rand_string);
    let server_netns_name = format!("ns-server-{}", rand_string);
    let rattan_netns_name = format!("ns-rattan-{}", rand_string);
    let client_netns = NetNs::new(&client_netns_name)?;
    let server_netns = match config.mode {
        StdNetEnvMode::Compatible => NetNs::current()?,
        StdNetEnvMode::Isolated => NetNs::new(&server_netns_name)?,
        StdNetEnvMode::Container => NetNs::new(&server_netns_name)?,
    };
    let rattan_netns = NetNs::new(&rattan_netns_name)?;

    let veth_pair_client = VethPairBuilder::new()
        .name(
            format!("rc-left-{}", rand_string),
            format!("rc-right-{}", rand_string),
        )
        .namespace(Some(client_netns.clone()), Some(rattan_netns.clone()))
        .mac_addr(
            [0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2a].into(),
            [0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2b].into(),
        )
        .ip_addr(
            (IpAddr::V4(Ipv4Addr::new(192, 168, 11, 1)), 24),
            (IpAddr::V4(Ipv4Addr::new(192, 168, 11, 2)), 24),
        )
        .build()?;

    let veth_pair_server = VethPairBuilder::new()
        .name(
            format!("rs-left-{}", rand_string),
            format!("rs-right-{}", rand_string),
        )
        .namespace(Some(rattan_netns.clone()), Some(server_netns.clone()))
        .mac_addr(
            [0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2c].into(),
            [0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2d].into(),
        )
        .ip_addr(
            (IpAddr::V4(Ipv4Addr::new(192, 168, 12, 2)), 24),
            (IpAddr::V4(Ipv4Addr::new(192, 168, 12, 1)), 24),
        )
        .build()?;

    // Set the default route of left and right namespaces
    let mut client_exec_handle = std::process::Command::new("ip")
        .args([
            "netns",
            "exec",
            &client_netns_name,
            "ip",
            "route",
            "add",
            "default",
            "via",
            "192.168.11.1",
        ])
        .spawn()
        .unwrap();
    match config.mode {
        StdNetEnvMode::Compatible => {
            let mut server_exec_handle = std::process::Command::new("ip")
                .args(["route", "add", "192.168.11.0/24", "via", "192.168.12.1"])
                .spawn()
                .unwrap();
            server_exec_handle.wait()?;
        }
        _ => {
            let mut server_exec_handle = std::process::Command::new("ip")
                .args([
                    "netns",
                    "exec",
                    &server_netns_name,
                    "ip",
                    "route",
                    "add",
                    "default",
                    "via",
                    "192.168.12.1",
                ])
                .spawn()
                .unwrap();
            server_exec_handle.wait()?;
        }
    }
    client_exec_handle.wait()?;

    if std::env::var("TEST_STD_NS").is_ok() {
        let output = std::process::Command::new("ip")
            .arg("netns")
            .arg("list")
            .output()
            .unwrap();
        println!(
            "ip netns list:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );

        for ns in &[&client_netns_name, &server_netns_name, &rattan_netns_name] {
            let output = std::process::Command::new("ip")
                .args(["netns", "exec", ns, "ip", "addr", "show"])
                .output()
                .unwrap();

            println!(
                "ip netns exec {} ip link list:\n{}",
                ns,
                String::from_utf8_lossy(&output.stdout)
            );

            let output = std::process::Command::new("ip")
                .args(["netns", "exec", ns, "ip", "-4", "route", "show"])
                .output()
                .unwrap();

            println!(
                "ip netns exec {} ip -4 route show:\n{}",
                ns,
                String::from_utf8_lossy(&output.stdout)
            );
        }

        let output = std::process::Command::new("ip")
            .args([
                "netns",
                "exec",
                &rattan_netns_name,
                "ping",
                "-c",
                "3",
                "192.168.11.1",
            ])
            .output()
            .unwrap();

        println!(
            "ip netns exec {} ping -c 3 192.168.11.1:\n{}",
            &rattan_netns_name,
            String::from_utf8_lossy(&output.stdout)
        );

        let output = std::process::Command::new("ip")
            .args([
                "netns",
                "exec",
                &rattan_netns_name,
                "ping",
                "-c",
                "3",
                "192.168.12.1",
            ])
            .output()
            .unwrap();

        println!(
            "ip netns exec {} ping -c 3 192.168.12.1:\n{}",
            &rattan_netns_name,
            String::from_utf8_lossy(&output.stdout)
        );
    }

    Ok(StdNetEnv {
        left_ns: client_netns,
        rattan_ns: rattan_netns,
        right_ns: server_netns,
        left_pair: veth_pair_client,
        right_pair: veth_pair_server,
    })
}
