use std::net::{IpAddr, Ipv4Addr};

use rattan::netns::NetNs;
use rattan::veth::VethPair;

//   ns-client                          ns-rattan                         ns-server
// +-----------+    veth pair    +--------------------+    veth pair    +-----------+
// |    rc-left| <-------------> |rc-right [P] rs-left| <-------------> |rs-right   |
// +-----------+                 +--------------------+                 +-----------+

fn main() -> anyhow::Result<()> {
    let client_netns = NetNs::new("ns-client")?;
    let server_netns = NetNs::new("ns-server")?;
    let rattan_netns = NetNs::new("ns-rattan")?;
    let mut veth_pair_client = VethPair::new("rc-left", "rc-right")?;

    veth_pair_client
        .left
        .set_ns("ns-client")?
        .set_l2_addr([0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2a].into())?
        .set_l3_addr(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 24)?
        .up()?;

    veth_pair_client
        .right
        .set_ns("ns-rattan")?
        .set_l2_addr([0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2b].into())?
        .set_l3_addr(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2)), 24)?
        .up()?;

    let mut veth_pair_server = VethPair::new("rs-left", "rs-right")?;
    veth_pair_server
        .left
        .set_ns("ns-rattan")?
        .set_l2_addr([0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2c].into())?
        .set_l3_addr(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1)), 24)?
        .up()?;

    veth_pair_server
        .right
        .set_ns("ns-server")?
        .set_l2_addr([0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2d].into())?
        .set_l3_addr(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 2)), 24)?
        .up()?;

    let output = std::process::Command::new("ip")
        .arg("netns")
        .arg("list")
        .output()
        .unwrap();
    println!(
        "ip netns list:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    for ns in vec!["ns-client", "ns-server", "ns-rattan"] {
        let output = std::process::Command::new("ip")
            .args(["netns", "exec", ns, "ip", "addr", "show"])
            .output()
            .unwrap();

        println!(
            "ip netns exec {} ip link list:\n{}",
            ns,
            String::from_utf8_lossy(&output.stdout)
        );
    }

    let output = std::process::Command::new("ip")
        .arg("netns")
        .arg("exec")
        .arg("ns-rattan")
        .arg("ping")
        .arg("-c")
        .arg("3")
        .arg("192.168.1.1")
        .output()
        .unwrap();

    println!(
        "ip netns exec ns-rattan ping -c 3 192.168.1.1:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let output = std::process::Command::new("ip")
        .arg("netns")
        .arg("exec")
        .arg("ns-rattan")
        .arg("ping")
        .arg("-c")
        .arg("3")
        .arg("192.168.2.2")
        .output()
        .unwrap();

    println!(
        "ip netns exec ns-rattan ping -c 3 192.168.2.2:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    std::mem::drop(veth_pair_client);
    std::mem::drop(veth_pair_server);
    client_netns.remove()?;
    server_netns.remove()?;
    rattan_netns.remove()?;
    Ok(())
}
