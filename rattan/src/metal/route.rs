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
use tracing::{debug, error, span, warn, Level};

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
        std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
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

pub fn add_route_with_netns<
    T: Into<Option<(IpAddr, u8)>>,
    U: Into<Option<IpAddr>>,
    V: Into<Option<u32>>,
>(
    dest: T,
    gateway: U,
    outif_id: V,
    netns: Arc<NetNs>,
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
                let mut handle = rtnl_handle.route().add().v4().destination_prefix(
                    Ipv4Net::new(dest_v4, prefix_length)
                        .map_err(|_| {
                            let msg = format!("IPv4 prefix length {} is invalid", prefix_length);
                            error!("{}", msg);
                            Error::ConfigError(msg)
                        })?
                        .trunc()
                        .addr(),
                    prefix_length,
                );
                if let Some(gateway) = gateway {
                    if let IpAddr::V4(gateway_v4) = gateway {
                        handle = handle.gateway(gateway_v4);
                    } else {
                        let msg = format!(
                            "dest {} and gateway {} are not the same type",
                            dest_v4, gateway
                        );
                        error!("{}", msg);
                        return Err(Error::ConfigError(msg.to_string()));
                    }
                }
                if let Some(if_id) = outif_id {
                    handle = handle.output_interface(if_id);
                }
                rt.block_on(handle.execute())
            }
            Some((IpAddr::V6(dest_v6), prefix_length)) => {
                let mut handle = rtnl_handle.route().add().v6().destination_prefix(
                    Ipv6Net::new(dest_v6, prefix_length)
                        .map_err(|_| {
                            let msg = format!("IPv6 prefix length {} is invalid", prefix_length);
                            error!("{}", msg);
                            Error::ConfigError(msg)
                        })?
                        .trunc()
                        .addr(),
                    prefix_length,
                );
                if let Some(gateway) = gateway {
                    if let IpAddr::V6(gateway_v6) = gateway {
                        handle = handle.gateway(gateway_v6);
                    } else {
                        let msg = format!(
                            "dest {} and gateway {} are not the same type",
                            dest_v6, gateway
                        );
                        error!("{}", msg);
                        return Err(Error::ConfigError(msg.to_string()));
                    }
                }
                if let Some(if_id) = outif_id {
                    handle = handle.output_interface(if_id);
                }
                rt.block_on(handle.execute())
            }
            None => {
                let mut handle = rtnl_handle.route().add();
                if let Some(if_id) = outif_id {
                    handle = handle.output_interface(if_id);
                }
                match gateway {
                    Some(IpAddr::V4(gateway_v4)) => {
                        rt.block_on(handle.v4().gateway(gateway_v4).execute())
                    }
                    Some(IpAddr::V6(gateway_v6)) => {
                        rt.block_on(handle.v6().gateway(gateway_v6).execute())
                    }
                    _ => {
                        let res = rt.block_on(handle.v4().execute());
                        if res.is_ok() {
                            let mut handle = rtnl_handle.route().add();
                            if let Some(if_id) = outif_id {
                                handle = handle.output_interface(if_id);
                            }
                            rt.block_on(handle.v6().execute())
                        } else {
                            res
                        }
                    }
                }
            }
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
    device_index: u32,
    netns: Arc<NetNs>,
) -> Result<(), Error> {
    debug!(?dest, ?mac, ?netns, ?device_index, "Add arp entry");
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
