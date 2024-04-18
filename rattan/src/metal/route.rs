use std::net::IpAddr;
use std::sync::Arc;

use crate::error::Error;

use super::{
    netns::{NetNs, NetNsGuard},
    veth::MacAddr,
};
use futures::TryStreamExt;
use ipnet::{Ipv4Net, Ipv6Net};
use netlink_packet_route::link::{LinkAttribute, LinkLayerType};
use tracing::{debug, error, span, trace, warn, Level};

fn execute_rtnetlink_with_new_thread<F>(netns: Arc<NetNs>, f: F) -> Result<(), Error>
where
    F: FnOnce(tokio::runtime::Runtime, rtnetlink::Handle) -> Result<(), Error> + Send + 'static,
{
    let build_thread_span = span!(Level::DEBUG, "build_thread").or_current();
    let build_thread = std::thread::spawn(move || -> Result<(), Error> {
        let _entered = build_thread_span.entered();
        let _ns_guard = NetNsGuard::new(netns.clone()).map_err(|e| {
            error!("Failed to enter netns: {}", e);
            e
        })?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                error!("Failed to build rtnetlink runtime: {:?}", e);
                Error::TokioRuntimeError(e.into())
            })?;
        let _guard = rt.enter();
        let (conn, rtnl_handle, _) = rtnetlink::new_connection()?;
        rt.spawn(conn);
        f(rt, rtnl_handle)
    });
    build_thread.join().unwrap()
}

pub fn add_gateway_with_netns(gateway: IpAddr, netns: Arc<NetNs>) -> Result<(), Error> {
    trace!(?gateway, ?netns, "Add gateway");
    execute_rtnetlink_with_new_thread(netns, move |rt, rtnl_handle| {
        match gateway {
            IpAddr::V4(gateway) => {
                rt.block_on(rtnl_handle.route().add().v4().gateway(gateway).execute())
            }
            IpAddr::V6(gateway) => {
                rt.block_on(rtnl_handle.route().add().v6().gateway(gateway).execute())
            }
        }
        .map(|_| {
            debug!("Add gateway {} successfully", gateway);
        })
        .map_err(|e| {
            error!("Failed to add gateway: {}", e);
            Error::MetalError(e.into())
        })
    })
}

pub fn add_route_with_netns(
    dest: IpAddr,
    prefix_length: u8,
    gateway: IpAddr,
    netns: Arc<NetNs>,
) -> Result<(), Error> {
    trace!(?dest, ?prefix_length, ?gateway, ?netns, "Add route");
    execute_rtnetlink_with_new_thread(netns, move |rt, rtnl_handle| {
        match (dest, gateway) {
            (IpAddr::V4(dest), IpAddr::V4(gateway)) => rt.block_on(
                rtnl_handle
                    .route()
                    .add()
                    .v4()
                    .destination_prefix(
                        Ipv4Net::new(dest, prefix_length)
                            .map_err(|_| {
                                let msg =
                                    format!("IPv4 prefix length {} is invalid", prefix_length);
                                error!("{}", msg);
                                Error::ConfigError(msg)
                            })?
                            .trunc()
                            .addr(),
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
                        Ipv6Net::new(dest, prefix_length)
                            .map_err(|_| {
                                let msg =
                                    format!("IPv6 prefix length {} is invalid", prefix_length);
                                error!("{}", msg);
                                Error::ConfigError(msg)
                            })?
                            .trunc()
                            .addr(),
                        prefix_length,
                    )
                    .gateway(gateway)
                    .execute(),
            ),
            _ => {
                let msg = format!(
                    "dest {} and gateway {} are not the same type",
                    dest, gateway
                );
                error!("{}", msg);
                return Err(Error::ConfigError(msg));
            }
        }
        .map(|_| {
            debug!("Add route {} via {} successfully", dest, gateway);
        })
        .map_err(|e| {
            error!("Failed to add route: {}", e);
            Error::MetalError(e.into())
        })
    })
}

pub fn add_arp_entry_with_netns(
    dest: IpAddr,
    mac: MacAddr,
    device_index: u32,
    netns: Arc<NetNs>,
) -> Result<(), Error> {
    trace!(?dest, ?mac, ?netns, ?device_index, "Add arp entry");
    execute_rtnetlink_with_new_thread(netns, move |rt, rtnl_handle| {
        rt.block_on(
            rtnl_handle
                .neighbours()
                .add(device_index, dest)
                .link_local_address(&mac.bytes())
                .execute(),
        )
        .map(|_| {
            debug!("Add arp entry {} -> {} successfully", dest, mac);
        })
        .map_err(|e| {
            error!("Failed to add arp entry: {}", e);
            Error::MetalError(e.into())
        })
    })
}

pub fn set_loopback_up_with_netns(netns: Arc<NetNs>) -> Result<(), Error> {
    trace!(?netns, "Set loopback interface up");
    execute_rtnetlink_with_new_thread(netns, move |rt, rtnl_handle| {
        let mut links = rtnl_handle.link().get().execute();
        rt.block_on(async {
            while let Some(msg) = links.try_next().await.unwrap() {
                if msg.header.link_layer_type == LinkLayerType::Loopback {
                    debug!(
                        id = msg.header.index,
                        name = msg.attributes.iter().find_map(|attr| {
                            if let LinkAttribute::IfName(name) = attr {
                                Some(name)
                            } else {
                                None
                            }
                        }),
                        "Try to set interface with Loopback type up"
                    );
                    return rtnl_handle
                        .link()
                        .set(msg.header.index)
                        .up()
                        .execute()
                        .await
                        .map(|_| {
                            debug!("Set loopback up successfully");
                        })
                        .map_err(|e| {
                            error!("Failed to set loopback up: {}", e);
                            Error::MetalError(e.into())
                        });
                }
            }
            warn!("Loopback interface not found");
            Ok(())
        })
    })
}
