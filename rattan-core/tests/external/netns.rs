/// This test need to be run as root (CAP_NET_ADMIN and CAP_SYS_ADMIN)
/// CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test netns -- --ignored --nocapture
use std::ops::{Deref, DerefMut};

use netns_rs::NetNs;
use tokio::runtime;

pub struct NetworkNs {
    pub inner: NetNs,
}

impl NetworkNs {
    pub fn close(self) {
        self.inner.remove().unwrap();
    }
}

impl Deref for NetworkNs {
    type Target = NetNs;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for NetworkNs {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

macro_rules! check_output {
    // when the status of output is not success, panic with the stderr
    ($output:expr) => {
        if !$output.status.success() {
            panic!(
                "ip command failed: {}",
                String::from_utf8_lossy(&$output.stderr)
            );
        }
    };
}

#[test]
#[ignore]
fn netns_test() {
    let rt = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let netns_left = NetworkNs {
            inner: NetNs::new("ns-left").unwrap(),
        };
        let netns_right = NetworkNs {
            inner: NetNs::new("ns-right").unwrap(),
        };
        let left_veth = "test-veth-left";
        let right_veth = "test-veth-right";
        let output = std::process::Command::new("ip")
            .arg("link")
            .arg("add")
            .arg(left_veth)
            .arg("type")
            .arg("veth")
            .arg("peer")
            .arg("name")
            .arg(right_veth)
            .output()
            .unwrap();
        check_output!(output);
        let output = std::process::Command::new("ip")
            .arg("link")
            .arg("set")
            .arg(left_veth)
            .arg("netns")
            .arg("ns-left")
            .output()
            .unwrap();
        check_output!(output);
        let output = std::process::Command::new("ip")
            .arg("link")
            .arg("set")
            .arg(right_veth)
            .arg("netns")
            .arg("ns-right")
            .output()
            .unwrap();
        check_output!(output);

        // in left network namespace
        netns_left
            .run(|_| {
                let output = std::process::Command::new("ip")
                    .arg("addr")
                    .arg("add")
                    .arg("192.168.11.1/24")
                    .arg("dev")
                    .arg(left_veth)
                    .output()
                    .unwrap();
                check_output!(output);
                let output = std::process::Command::new("ip")
                    .arg("link")
                    .arg("set")
                    .arg(left_veth)
                    .arg("up")
                    .output()
                    .unwrap();
                check_output!(output);
            })
            .unwrap();

        // in right network namespace
        netns_right
            .run(|_| {
                let output = std::process::Command::new("ip")
                    .arg("addr")
                    .arg("add")
                    .arg("192.168.1.2/24")
                    .arg("dev")
                    .arg(right_veth)
                    .output()
                    .unwrap();
                check_output!(output);
                let output = std::process::Command::new("ip")
                    .arg("link")
                    .arg("set")
                    .arg(right_veth)
                    .arg("up")
                    .output()
                    .unwrap();
                check_output!(output);
            })
            .unwrap();
        let output = std::process::Command::new("ip")
            .arg("netns")
            .arg("exec")
            .arg("ns-left")
            .arg("ping")
            .arg("-c")
            .arg("3")
            .arg("192.168.1.2")
            .output()
            .unwrap();
        println!("{}", String::from_utf8_lossy(&output.stdout));
        netns_left.close();
        netns_right.close();
    });
}
