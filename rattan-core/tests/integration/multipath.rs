/// This test need to be run as root (CAP_NET_ADMIN, CAP_SYS_ADMIN and CAP_SYS_RAW)
/// RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test loss --all-features -- --nocapture
use std::collections::HashMap;

use rattan_core::cells::router::RoutingEntry;
use rattan_core::cells::StdPacket;
use rattan_core::config::{CellBuildConfig, RattanConfig, RouterCellBuildConfig};
use rattan_core::env::{StdNetEnvConfig, StdNetEnvMode};
use rattan_core::metal::io::af_packet::AfPacketDriver;
use rattan_core::radix::RattanRadix;
use tracing::instrument;

use tracing::warn;

#[instrument]
#[test_log::test]
#[serial_test::parallel]
fn test_multipath_ping() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            left_veth_count: 4,
            right_veth_count: 2,
            client_cores: vec![1],
            server_cores: vec![3],
        },
        ..Default::default()
    };
    config.cells.insert(
        "router".to_string(),
        CellBuildConfig::Router(RouterCellBuildConfig {
            egress_connections: vec![
                "left1".to_string(),
                "left2".to_string(),
                "right1".to_string(),
                "right2".to_string(),
            ],
            routing_table: vec![
                RoutingEntry::new("10.1.1.1/32".parse().unwrap(), Some(0)),
                RoutingEntry::new("10.1.2.1/32".parse().unwrap(), Some(1)),
                RoutingEntry::new("10.2.1.1/32".parse().unwrap(), Some(2)),
                RoutingEntry::new("10.2.2.1/32".parse().unwrap(), Some(3)),
            ],
        }),
    );
    config.links = HashMap::from([
        ("left1".to_string(), "router".to_string()),
        ("left2".to_string(), "right2".to_string()),
        ("left3".to_string(), "router".to_string()),
        ("right1".to_string(), "router".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Wait for AfPacketDriver to be ready
    std::thread::sleep(std::time::Duration::from_millis(100));

    assert!(radix.ping_right_from_left(1, 1).unwrap());
    assert!(radix.ping_right_from_left(1, 2).unwrap());
    assert!(radix.ping_right_from_left(2, 1).unwrap());
    assert!(radix.ping_right_from_left(2, 2).unwrap());
    assert!(!radix.ping_right_from_left(3, 1).unwrap());
}
