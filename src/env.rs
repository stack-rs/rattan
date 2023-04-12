use crate::{netns::NetNs, veth::VethPair};
use std::net::{IpAddr, Ipv4Addr};

//   ns-client                          ns-rattan                         ns-server
// +-----------+    veth pair    +--------------------+    veth pair    +-----------+
// |    rc-left| <-------------> |rc-right [P] rs-left| <-------------> |rs-right   |
// |    .1.1/24|                 |.1.2/24      .2.2/24|                 |.2.1/24    |
// +-----------+                 +--------------------+                 +-----------+

pub struct StdNetEnv {
    pub left_ns: std::sync::Arc<NetNs>,
    pub rattan_ns: std::sync::Arc<NetNs>,
    pub right_ns: std::sync::Arc<NetNs>,
    pub left_pair: VethPair,
    pub right_pair: VethPair,
}

pub fn get_std_env() -> anyhow::Result<StdNetEnv> {
    let client_netns = NetNs::new("ns-client")?;
    let server_netns = NetNs::new("ns-server")?;
    let rattan_netns = NetNs::new("ns-rattan")?;
    let mut veth_pair_client = VethPair::new("rc-left", "rc-right")?;

    veth_pair_client
        .left
        .set_ns(client_netns.clone())?
        .set_l2_addr([0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2a].into())?
        .set_l3_addr(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 24)?
        .disable_checksum_offload()?
        .up()?;

    veth_pair_client
        .right
        .set_ns(rattan_netns.clone())?
        .set_l2_addr([0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2b].into())?
        .set_l3_addr(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2)), 24)?
        .disable_checksum_offload()?
        .up()?;

    let mut veth_pair_server = VethPair::new("rs-left", "rs-right")?;
    veth_pair_server
        .left
        .set_ns(rattan_netns.clone())?
        .set_l2_addr([0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2c].into())?
        .set_l3_addr(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 2)), 24)?
        .disable_checksum_offload()?
        .up()?;

    veth_pair_server
        .right
        .set_ns(server_netns.clone())?
        .set_l2_addr([0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2d].into())?
        .set_l3_addr(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1)), 24)?
        .disable_checksum_offload()?
        .up()?;

    // Set the default route of left and right namespaces
    std::process::Command::new("ip")
        .args([
            "netns",
            "exec",
            "ns-client",
            "ip",
            "route",
            "add",
            "default",
            "via",
            "192.168.1.1",
        ])
        .output()
        .unwrap();
    std::process::Command::new("ip")
        .args([
            "netns",
            "exec",
            "ns-server",
            "ip",
            "route",
            "add",
            "default",
            "via",
            "192.168.2.1",
        ])
        .output()
        .unwrap();

    let output = std::process::Command::new("ip")
        .arg("netns")
        .arg("list")
        .output()
        .unwrap();
    println!(
        "ip netns list:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    for ns in &["ns-client", "ns-server", "ns-rattan"] {
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
            "ns-rattan",
            "ping",
            "-c",
            "3",
            "192.168.1.1",
        ])
        .output()
        .unwrap();

    println!(
        "ip netns exec ns-rattan ping -c 3 192.168.1.1:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let output = std::process::Command::new("ip")
        .args([
            "netns",
            "exec",
            "ns-rattan",
            "ping",
            "-c",
            "3",
            "192.168.2.1",
        ])
        .output()
        .unwrap();

    println!(
        "ip netns exec ns-rattan ping -c 3 192.168.2.1:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    Ok(StdNetEnv {
        left_ns: client_netns,
        rattan_ns: rattan_netns,
        right_ns: server_netns,
        left_pair: veth_pair_client,
        right_pair: veth_pair_server,
    })
}
