use std::net::IpAddr;
use std::sync::Arc;

use super::{
    netns::{NetNs, NetNsGuard},
    veth::MacAddr,
};
use ipnet::{Ipv4Net, Ipv6Net};
use tracing::{debug, error, span, trace, Level};

fn execute_rtnetlink_with_new_thread<F>(netns: Arc<NetNs>, f: F)
where
    F: FnOnce(tokio::runtime::Runtime, rtnetlink::Handle) + Send + 'static,
{
    let build_thread_span = span!(Level::DEBUG, "build_thread").or_current();
    let build_thread = std::thread::spawn(move || {
        let _entered = build_thread_span.entered();
        let ns_guard = NetNsGuard::new(netns.clone());
        if let Err(e) = ns_guard {
            error!("Failed to enter netns: {}", e);
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = rt.enter();
        let (conn, rtnl_handle, _) = rtnetlink::new_connection().unwrap();
        rt.spawn(conn);
        f(rt, rtnl_handle);
    });
    build_thread.join().unwrap();
}

pub fn add_gateway_with_netns(gateway: IpAddr, netns: Arc<NetNs>) {
    trace!(?gateway, ?netns, "Add gateway");
    execute_rtnetlink_with_new_thread(netns, move |rt, rtnl_handle| {
        let result = match gateway {
            IpAddr::V4(gateway) => {
                rt.block_on(rtnl_handle.route().add().v4().gateway(gateway).execute())
            }
            IpAddr::V6(gateway) => {
                rt.block_on(rtnl_handle.route().add().v6().gateway(gateway).execute())
            }
        };
        match result {
            Ok(_) => {
                debug!("Add gateway {} successfully", gateway);
            }
            Err(e) => {
                error!("Failed to add gateway: {}", e);
                panic!("Failed to add gateway: {}", e);
            }
        }
    });
}

pub fn add_route_with_netns(dest: IpAddr, prefix_length: u8, gateway: IpAddr, netns: Arc<NetNs>) {
    trace!(?dest, ?prefix_length, ?gateway, ?netns, "Add route");
    execute_rtnetlink_with_new_thread(netns, move |rt, rtnl_handle| {
        let result = match (dest, gateway) {
            (IpAddr::V4(dest), IpAddr::V4(gateway)) => rt.block_on(
                rtnl_handle
                    .route()
                    .add()
                    .v4()
                    .destination_prefix(
                        Ipv4Net::new(dest, prefix_length).unwrap().trunc().addr(),
                        prefix_length,
                    )
                    .gateway(gateway)
                    .execute(),
            ),
            (IpAddr::V6(dest), IpAddr::V6(gateway)) => rt.block_on(
                rtnl_handle
                    .route()
                    .add()
                    .v6()
                    .destination_prefix(
                        Ipv6Net::new(dest, prefix_length).unwrap().trunc().addr(),
                        prefix_length,
                    )
                    .gateway(gateway)
                    .execute(),
            ),
            _ => {
                error!(?dest, ?gateway, "dest and gateway are not the same type");
                panic!("dest and gateway are not the same type");
            }
        };
        match result {
            Ok(_) => {
                debug!("Add route {} via {} successfully", dest, gateway);
            }
            Err(e) => {
                error!("Failed to add route: {}", e);
                panic!("Failed to add route: {}", e);
            }
        }
    });
}

pub fn add_arp_entry_with_netns(dest: IpAddr, mac: MacAddr, device_index: u32, netns: Arc<NetNs>) {
    trace!(?dest, ?mac, ?netns, ?device_index, "Add arp entry");
    execute_rtnetlink_with_new_thread(netns, move |rt, rtnl_handle| {
        let result = rt.block_on(
            rtnl_handle
                .neighbours()
                .add(device_index, dest)
                .link_local_address(&mac.bytes())
                .execute(),
        );
        match result {
            Ok(_) => {
                debug!("Add arp entry {} -> {} successfully", dest, mac);
            }
            Err(e) => {
                error!("Failed to add arp entry: {}", e);
                panic!("Failed to add arp entry: {}", e);
            }
        }
    });
}

pub fn set_link_up_with_netns(device_index: u32, netns: Arc<NetNs>) {
    trace!(?device_index, ?netns, "Set link up");
    execute_rtnetlink_with_new_thread(netns, move |rt, rtnl_handle| {
        let result = rt.block_on(rtnl_handle.link().set(device_index).up().execute());
        match result {
            Ok(_) => {
                debug!("Set link {} up successfully", device_index);
            }
            Err(e) => {
                error!("Failed to set link up: {}", e);
                panic!("Failed to set link up: {}", e);
            }
        }
    });
}
