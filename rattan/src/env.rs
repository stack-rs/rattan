use crate::metal::{
    netns::NetNs,
    route::{add_arp_entry_with_netns, add_gateway_with_netns, add_route_with_netns},
    veth::{VethPair, VethPairBuilder},
};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tracing::{debug, info, instrument, span, trace, Level};

//   ns-client                          ns-rattan                         ns-server
// +-----------+    veth pair    +--------------------+    veth pair    +-----------+
// |    rc-left| <-------------> |rc-right [P] rs-left| <-------------> |rs-right   |
// |   .11.1/24|                 |.11.2/24    .12.2/24|                 |.12.1/24   |
// +-----------+                 +--------------------+                 +-----------+

lazy_static::lazy_static! {
    static ref STD_ENV_LOCK: Arc<parking_lot::Mutex<()>> = Arc::new(parking_lot::Mutex::new(()));
}

#[derive(Debug)]
pub enum StdNetEnvMode {
    Compatible,
    Isolated,
    Container,
}

#[derive(Debug)]
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

#[instrument(skip_all, level = "debug")]
pub fn get_std_env(config: StdNetEnvConfig) -> anyhow::Result<StdNetEnv> {
    trace!(?config);
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
    trace!(?client_netns, "Client netns {} created", client_netns_name);
    let server_netns = match config.mode {
        StdNetEnvMode::Compatible => NetNs::current()?,
        StdNetEnvMode::Isolated => NetNs::new(&server_netns_name)?,
        StdNetEnvMode::Container => NetNs::new(&server_netns_name)?,
    };
    trace!(?server_netns, "Server netns {} created", server_netns_name);
    let rattan_netns = NetNs::new(&rattan_netns_name)?;
    trace!(?rattan_netns, "Rattan netns {} created", rattan_netns_name);

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
    info!("Set default route");

    debug!("Set default route for client namespace");
    add_gateway_with_netns(veth_pair_client.left.ip_addr.0, client_netns.clone());

    debug!("Set default route for server namespace");
    match config.mode {
        StdNetEnvMode::Compatible => {
            add_route_with_netns(
                veth_pair_client.left.ip_addr.0,
                veth_pair_client.left.ip_addr.1,
                veth_pair_server.right.ip_addr.0,
                server_netns.clone(),
            );
        }
        _ => {
            add_gateway_with_netns(veth_pair_server.right.ip_addr.0, server_netns.clone());
        }
    }

    // Set the default neighbours of left and right namespaces
    info!("Set default neighbours");

    debug!("Set default neighbours for client namespace");
    add_arp_entry_with_netns(
        veth_pair_server.right.ip_addr.0,
        veth_pair_server.right.mac_addr,
        veth_pair_client.left.index,
        client_netns.clone(),
    );

    debug!("Set default neighbours for server namespace");
    add_arp_entry_with_netns(
        veth_pair_client.left.ip_addr.0,
        veth_pair_client.left.mac_addr,
        veth_pair_server.right.index,
        server_netns.clone(),
    );

    if std::env::var("TEST_STD_NS").is_ok() {
        let _span = span!(Level::INFO, "TEST_STD_NS").entered();
        let output = std::process::Command::new("ip")
            .arg("netns")
            .arg("list")
            .output()
            .unwrap();
        info!(
            "ip netns list:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );

        for ns in &[&client_netns_name, &server_netns_name, &rattan_netns_name] {
            let output = std::process::Command::new("ip")
                .args(["netns", "exec", ns, "ip", "addr", "show"])
                .output()
                .unwrap();

            info!(
                "ip netns exec {} ip link list:\n{}",
                ns,
                String::from_utf8_lossy(&output.stdout)
            );

            let output = std::process::Command::new("ip")
                .args(["netns", "exec", ns, "ip", "-4", "route", "show"])
                .output()
                .unwrap();

            info!(
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

        info!(
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

        info!(
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
