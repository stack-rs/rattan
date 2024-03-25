use crate::metal::{
    netns::NetNs,
    route::{
        add_arp_entry_with_netns, add_gateway_with_netns, add_route_with_netns,
        set_loopback_up_with_netns,
    },
    veth::{MacAddr, VethDevice, VethPair, VethPairBuilder},
};
use futures::TryStreamExt;
use netlink_packet_route::{address::AddressAttribute, link::LinkAttribute};
use once_cell::sync::OnceCell;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::sync::Arc;
use std::{
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
};
use tracing::{debug, error, info, instrument, span, trace, Level};

//   ns-client                          ns-rattan                         ns-server
// +-----------+    veth pair    +--------------------+    veth pair    +-----------+
// |    rc-left| <-------------> |rc-right [P] rs-left| <-------------> |rs-right   |
// |   .11.x/32|                 |.11.2/32    .12.2/32|                 |.12.x/32   |
// +-----------+                 +--------------------+                 +-----------+

fn get_addresses_in_use() -> Vec<IpAddr> {
    debug!("Get addresses in use");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();
    let (conn, rtnl_handle, _) = rtnetlink::new_connection().unwrap();
    rt.spawn(conn);

    let mut addresses = vec![];
    rt.block_on(async {
        let mut links = rtnl_handle.address().get().execute();
        while let Ok(Some(address_msg)) = links.try_next().await {
            for address_attr in address_msg.attributes {
                if let AddressAttribute::Address(address) = address_attr {
                    debug!(?address, ?address_msg.header.prefix_len, "Get address");
                    addresses.push(address);
                }
            }
        }
    });
    debug!(?addresses, "Addresses in use");
    addresses
}

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
    get_addresses_in_use();
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

    // Get server veth address
    let veth_addr_suffix = match config.mode {
        StdNetEnvMode::Compatible => {
            let addresses_in_use = get_addresses_in_use();
            let mut addr_suffix = 1;
            while addresses_in_use.contains(&IpAddr::V4(Ipv4Addr::new(192, 168, 12, addr_suffix)))
                || addresses_in_use.contains(&IpAddr::V4(Ipv4Addr::new(192, 168, 11, addr_suffix)))
            {
                addr_suffix += 1;
                if addr_suffix == 2 {
                    addr_suffix += 1;
                }
                if addr_suffix == 255 {
                    error!("No available address suffix for server veth");
                    return Err(anyhow::anyhow!(
                        "No available address suffix for server veth"
                    ));
                }
            }
            addr_suffix
        }
        _ => 1,
    };
    let veth_pair_client = VethPairBuilder::new()
        .name(
            format!("rc-left-{}", rand_string),
            format!("rc-right-{}", rand_string),
        )
        .namespace(Some(client_netns.clone()), Some(rattan_netns.clone()))
        .mac_addr(
            [0x38, 0x7e, 0x58, 0xe7, 11, veth_addr_suffix].into(),
            [0x38, 0x7e, 0x58, 0xe7, 11, 2].into(),
        )
        .ip_addr(
            (
                IpAddr::V4(Ipv4Addr::new(192, 168, 11, veth_addr_suffix)),
                32,
            ),
            (IpAddr::V4(Ipv4Addr::new(192, 168, 11, 2)), 32),
        )
        .build()?;

    let veth_pair_server = VethPairBuilder::new()
        .name(
            format!("rs-left-{}", rand_string),
            format!("rs-right-{}", rand_string),
        )
        .namespace(Some(rattan_netns.clone()), Some(server_netns.clone()))
        .mac_addr(
            [0x38, 0x7e, 0x58, 0xe7, 12, 2].into(),
            [0x38, 0x7e, 0x58, 0xe7, 12, veth_addr_suffix].into(),
        )
        .ip_addr(
            (IpAddr::V4(Ipv4Addr::new(192, 168, 12, 2)), 32),
            (
                IpAddr::V4(Ipv4Addr::new(192, 168, 12, veth_addr_suffix)),
                32,
            ),
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

    debug!("Set lo interface up");
    set_loopback_up_with_netns(client_netns.clone());
    set_loopback_up_with_netns(rattan_netns.clone());
    set_loopback_up_with_netns(server_netns.clone());

    Ok(StdNetEnv {
        left_ns: client_netns,
        rattan_ns: rattan_netns,
        right_ns: server_netns,
        left_pair: veth_pair_client,
        right_pair: veth_pair_server,
    })
}

#[derive(Debug)]
pub struct ContainerEnv {
    pub veth_list: Vec<VethDevice>,
    pub fake_peer: Arc<VethDevice>,
}

#[instrument(skip_all, level = "debug")]
pub fn get_container_env() -> anyhow::Result<ContainerEnv> {
    debug!("Getting all veth devices");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();
    let (conn, rtnl_handle, _) = rtnetlink::new_connection().unwrap();
    rt.spawn(conn);

    // Get all link device
    let mut links = rtnl_handle.link().get().execute();
    let mut veth_list = vec![];
    rt.block_on(async {
        while let Some(msg) = links.try_next().await.unwrap() {
            let mut name: Option<String> = None;
            let index = msg.header.index;
            let mut mac_addr: Option<MacAddr> = None;
            let mut ip_addr: Option<(IpAddr, u8)> = None;
            let namespace: Arc<NetNs> = NetNs::current().unwrap();

            // Get link attributes
            for link_attr in msg.attributes {
                match link_attr {
                    LinkAttribute::IfName(n) => {
                        name = Some(n);
                    }
                    LinkAttribute::Address(m) => {
                        mac_addr = Some(MacAddr::new(m.try_into().unwrap()));
                    }
                    _ => {}
                }
            }

            // Skip lo devices
            if let (Some(name), Some(mac_addr)) = (name, mac_addr) {
                if name.starts_with("lo") {
                    continue;
                }
                // Get ip address attributes
                if let Ok(Some(address_msg)) = rtnl_handle
                    .address()
                    .get()
                    .set_link_index_filter(index)
                    .execute()
                    .try_next()
                    .await
                {
                    for address_attr in address_msg.attributes {
                        if let AddressAttribute::Address(address) = address_attr {
                            ip_addr = Some((address, address_msg.header.prefix_len));
                        }
                    }
                    if ip_addr.is_none() {
                        continue;
                    }

                    let veth = VethDevice {
                        name,
                        index,
                        mac_addr,
                        ip_addr: ip_addr.unwrap(),
                        peer: OnceCell::new(),
                        namespace,
                    };

                    debug!(?veth, "Get veth device");
                    veth_list.push(veth);
                }
            }
        }
    });

    // FIXME: add arp for each veth
    veth_list.sort_by(|a, b| a.index.cmp(&b.index));

    // FIXME: fake a peer veth device to get mac address
    let mut fake_peer = veth_list[0].clone();
    fake_peer.mac_addr = MacAddr::from_str("ff:ff:ff:ff:ff:ff").unwrap();
    let fake_peer = Arc::new(fake_peer);
    for veth in &veth_list {
        veth.peer.set(Arc::downgrade(&fake_peer)).unwrap();
    }
    Ok(ContainerEnv {
        veth_list,
        fake_peer,
    })
}
