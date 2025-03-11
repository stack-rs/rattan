use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use crate::error::Error;

use super::{
    netns::{NetNs, NetNsGuard},
    veth::MacAddr,
};
use futures::TryStreamExt;
use ipnet::{Ipv4Net, Ipv6Net};
use rtnetlink::packet_route::{
    link::{LinkAttribute, LinkLayerType},
    route::RouteScope,
};
use rtnetlink::{LinkMessageBuilder, LinkUnspec, RouteMessageBuilder};
use tracing::{debug, error, span, warn, Level};

fn execute_rtnetlink_with_new_thread<F>(netns: Arc<NetNs>, f: F) -> Result<(), Error>
where
    F: FnOnce(tokio::runtime::Runtime, rtnetlink::Handle) -> Result<(), Error> + Send + 'static,
{
    let build_thread_span = span!(Level::DEBUG, "build_thread").or_current();
    let build_thread = std::thread::spawn(move || -> Result<(), Error> {
        let _entered = build_thread_span.entered();
        let _ns_guard = NetNsGuard::new(netns.clone())?;
        std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::TokioRuntimeError(e.into()))?;
        let _guard = rt.enter();
        let (conn, rtnl_handle, _) = rtnetlink::new_connection()?;
        rt.spawn(conn);
        f(rt, rtnl_handle)
    });
    build_thread.join().unwrap()
}

pub fn add_route_with_netns<
    T: Into<Option<(IpAddr, u8)>>,
    U: Into<Option<IpAddr>>,
    V: Into<Option<u32>>,
>(
    dest: T,
    gateway: U,
    outif_id: V,
    netns: Arc<NetNs>,
    scope: RouteScope,
) -> Result<(), Error> {
    let dest = dest.into();
    let gateway = gateway.into();
    let outif_id = outif_id.into();
    debug!(?dest, ?gateway, ?outif_id, ?netns, "Add route");
    if gateway.is_none() && outif_id.is_none() {
        return Err(Error::ConfigError(
            "gateway and outif_id cannot be both None".to_string(),
        ));
    }
    execute_rtnetlink_with_new_thread(netns, move |rt, rtnl_handle| {
        match dest {
            Some((IpAddr::V4(dest_v4), prefix_length)) => {
                let mut message = RouteMessageBuilder::<Ipv4Addr>::new()
                    .scope(scope)
                    .destination_prefix(
                        Ipv4Net::new(dest_v4, prefix_length)
                            .map_err(|_| {
                                Error::ConfigError(format!(
                                    "IPv4 prefix length {} is invalid",
                                    prefix_length
                                ))
                            })?
                            .trunc()
                            .addr(),
                        prefix_length,
                    );
                if let Some(gateway) = gateway {
                    if let IpAddr::V4(gateway_v4) = gateway {
                        message = message.gateway(gateway_v4);
                    } else {
                        return Err(Error::ConfigError(format!(
                            "dest {} and gateway {} are not the same type",
                            dest_v4, gateway
                        )));
                    }
                }
                if let Some(if_id) = outif_id {
                    message = message.output_interface(if_id);
                }
                rt.block_on(rtnl_handle.route().add(message.build()).execute())
            }
            Some((IpAddr::V6(dest_v6), prefix_length)) => {
                let mut message = RouteMessageBuilder::<Ipv6Addr>::new()
                    .scope(scope)
                    .destination_prefix(
                        Ipv6Net::new(dest_v6, prefix_length)
                            .map_err(|_| {
                                Error::ConfigError(format!(
                                    "IPv6 prefix length {} is invalid",
                                    prefix_length
                                ))
                            })?
                            .trunc()
                            .addr(),
                        prefix_length,
                    );
                if let Some(gateway) = gateway {
                    if let IpAddr::V6(gateway_v6) = gateway {
                        message = message.gateway(gateway_v6);
                    } else {
                        return Err(Error::ConfigError(format!(
                            "dest {} and gateway {} are not the same type",
                            dest_v6, gateway
                        )));
                    }
                }
                if let Some(if_id) = outif_id {
                    message = message.output_interface(if_id);
                }
                rt.block_on(rtnl_handle.route().add(message.build()).execute())
            }
            None => match gateway {
                Some(IpAddr::V4(gateway_v4)) => {
                    let mut message = RouteMessageBuilder::<Ipv4Addr>::new()
                        .scope(scope)
                        .gateway(gateway_v4);
                    if let Some(if_id) = outif_id {
                        message = message.output_interface(if_id);
                    }
                    rt.block_on(rtnl_handle.route().add(message.build()).execute())
                }
                Some(IpAddr::V6(gateway_v6)) => {
                    let mut message = RouteMessageBuilder::<Ipv6Addr>::new()
                        .scope(scope)
                        .gateway(gateway_v6);
                    if let Some(if_id) = outif_id {
                        message = message.output_interface(if_id);
                    }
                    rt.block_on(rtnl_handle.route().add(message.build()).execute())
                }
                _ => {
                    let mut message = RouteMessageBuilder::<Ipv4Addr>::new().scope(scope);
                    if let Some(if_id) = outif_id {
                        message = message.output_interface(if_id);
                    }
                    let res = rt.block_on(rtnl_handle.route().add(message.build()).execute());
                    if res.is_ok() {
                        let mut message = RouteMessageBuilder::<Ipv6Addr>::new().scope(scope);
                        if let Some(if_id) = outif_id {
                            message = message.output_interface(if_id);
                        }
                        rt.block_on(rtnl_handle.route().add(message.build()).execute())
                    } else {
                        res
                    }
                }
            },
        }
        .map(|_| {
            debug!(?dest, ?gateway, ?outif_id, "Add route successfully");
        })
        .map_err(|e| {
            error!(
                "Failed to add route (add {:?} via {:?} dev {:?}): {}",
                dest, gateway, outif_id, e
            );
            // XXX: Debug command: ip route
            let output = std::process::Command::new("ip")
                .arg("route")
                .output()
                .map(|output| {
                    String::from_utf8_lossy(&output.stdout).to_string()
                        + String::from_utf8_lossy(&output.stderr).as_ref()
                })
                .unwrap_or_else(|e| e.to_string());
            println!("ip route: {}", output);
            Error::MetalError(e.into())
        })
    })
}

pub fn add_arp_entry_with_netns(
    dest: IpAddr,
    mac: MacAddr,
    cell_index: u32,
    netns: Arc<NetNs>,
) -> Result<(), Error> {
    debug!(?dest, ?mac, ?netns, ?cell_index, "Add arp entry");
    execute_rtnetlink_with_new_thread(netns, move |rt, rtnl_handle| {
        rt.block_on(
            rtnl_handle
                .neighbours()
                .add(cell_index, dest)
                .link_local_address(&mac.bytes())
                .execute(),
        )
        .map(|_| {
            debug!("Add arp entry {} -> {} successfully", dest, mac);
        })
        .map_err(|e| {
            error!("Failed to add arp entry: {}", e);
            // XXX: Debug command: arp -n
            let output = std::process::Command::new("arp")
                .arg("-n")
                .output()
                .map(|output| {
                    String::from_utf8_lossy(&output.stdout).to_string()
                        + String::from_utf8_lossy(&output.stderr).as_ref()
                })
                .unwrap_or_else(|e| e.to_string());
            println!("arp -n: {}", output);
            Error::MetalError(e.into())
        })
    })
}

pub fn set_loopback_up_with_netns(netns: Arc<NetNs>) -> Result<(), Error> {
    debug!(?netns, "Set loopback interface up");
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
                    let message = LinkMessageBuilder::<LinkUnspec>::new()
                        .index(msg.header.index)
                        .up()
                        .build();
                    return rtnl_handle
                        .link()
                        .set(message)
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
