use std::io;
use std::{ffi::CString, net::IpAddr};

use nftnl::{nft_expr, Batch, Chain, ChainType, FinalizedBatch, ProtoFamily, Rule, Table};
use tracing::{debug, warn};

/// Manages NAT rules using nftables.
///
/// This struct handles the creation and deletion of a dedicated nftables table
/// for NAT purposes. It uses RAII to ensure the table is deleted when the
/// struct is dropped.
pub struct Nat {
    table_name: String,
}

impl Nat {
    /// Create a new NAT manager.
    ///
    /// This will create a new nftables table with a unique name based on the process ID.
    /// It sets up prerouting and postrouting chains to perform NAT for traffic coming
    /// from the specified ingress address.
    pub fn new(ingress_addr: IpAddr) -> Self {
        // Use PID to create a unique table name to avoid conflicts with other instances
        let pid = std::process::id();
        let table_name_str = format!("rattan_nat_{}", pid);
        let table_name = CString::new(table_name_str.clone()).unwrap();

        let ingress_addr = match ingress_addr {
            IpAddr::V4(ipv4) => ipv4,
            IpAddr::V6(_) => {
                warn!("IPv6 NAT is not supported now. Skipping NAT setup.");
                return Self {
                    table_name: String::new(),
                };
            }
        };

        let mut batch = Batch::new();

        // Add Table
        // We use the IPv4 family for this table
        let table = Table::new(&table_name, ProtoFamily::Ipv4);
        batch.add(&table, nftnl::MsgType::Add);

        // Add Prerouting Chain
        // Hook: Prerouting, Priority: -100 (before routing decision)
        let mut pre_chain = Chain::new(c"prerouting", &table);
        pre_chain.set_hook(nftnl::Hook::PreRouting, -100);
        pre_chain.set_type(ChainType::Nat);
        pre_chain.set_policy(nftnl::Policy::Accept);
        batch.add(&pre_chain, nftnl::MsgType::Add);

        // Add Postrouting Chain
        // Hook: Postrouting, Priority: 100 (after routing decision)
        let mut post_chain = Chain::new(c"postrouting", &table);
        post_chain.set_hook(nftnl::Hook::PostRouting, 100);
        post_chain.set_type(ChainType::Nat);
        post_chain.set_policy(nftnl::Policy::Accept);
        batch.add(&post_chain, nftnl::MsgType::Add);

        // Add Prerouting Rule: ip saddr <ingress_addr> ct mark set <pid>
        // This marks packets coming from the ingress IP with our PID.
        // This mark is later used to identify packets that need masquerading.
        let mut pre_rule = Rule::new(&pre_chain);
        pre_rule.add_expr(&nft_expr!(payload ipv4 saddr));
        pre_rule.add_expr(&nft_expr!(cmp == ingress_addr));
        pre_rule.add_expr(&nft_expr!(immediate data pid));
        pre_rule.add_expr(&nft_expr!(ct mark set));
        batch.add(&pre_rule, nftnl::MsgType::Add);

        // Add Postrouting Rule: ct mark <pid> masquerade
        // This performs masquerading (SNAT) for packets that were marked in the prerouting chain.
        let mut post_rule = Rule::new(&post_chain);
        post_rule.add_expr(&nft_expr!(ct mark));
        post_rule.add_expr(&nft_expr!(cmp == pid));
        post_rule.add_expr(&nft_expr!(masquerade));
        batch.add(&post_rule, nftnl::MsgType::Add);

        let finalized_batch = batch.finalize();
        debug!("Applying nftables ruleset for table {}", table_name_str);

        if let Err(e) = send_and_process(&finalized_batch) {
            warn!("Failed to apply nftables rules: {}", e);
        }

        Self {
            table_name: table_name_str,
        }
    }
}

impl Drop for Nat {
    // Clean up the nftables table when the Nat struct is dropped (RAII)
    fn drop(&mut self) {
        if self.table_name.is_empty() {
            // NAT table was not created, nothing to clean up
            return;
        }

        let table_name = CString::new(self.table_name.clone()).unwrap();
        let mut batch = Batch::new();

        let table = Table::new(&table_name, ProtoFamily::Ipv4);
        batch.add(&table, nftnl::MsgType::Del);

        let finalized_batch = batch.finalize();
        debug!("Deleting nftables table: {}", self.table_name);

        if let Err(e) = send_and_process(&finalized_batch) {
            warn!("Failed to delete nftables table: {}", e);
        }
    }
}

/// Helper function to send a batch of netlink messages and process the responses (ACKs).
fn send_and_process(batch: &FinalizedBatch) -> io::Result<()> {
    let socket = mnl::Socket::new(mnl::Bus::Netfilter)?;
    let portid = socket.portid();

    socket.send_all(batch)?;

    let mut buffer = vec![0; nftnl::nft_nlmsg_maxsize() as usize];
    for message in socket.recv(&mut buffer[..])? {
        let message = message?;
        match mnl::cb_run(message, 0, portid)? {
            mnl::CbResult::Stop => {
                break;
            }
            mnl::CbResult::Ok => {}
        };
    }
    Ok(())
}
