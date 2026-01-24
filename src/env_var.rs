use std::net::IpAddr;
use std::process::Command;

pub fn add_runtime_env_var(
    handle: &mut Command,
    ip_list: Vec<(usize, IpAddr)>,
    rattan_id: impl Into<String>,
) {
    for (i, ip) in ip_list.into_iter() {
        handle.env(format!("RATTAN_IP_{i}"), ip.to_string());
        match i {
            0 => {
                handle.env("RATTAN_EXT", ip.to_string());
            }
            1 => {
                handle.env("RATTAN_BASE", ip.to_string());
            }
            _ => {}
        };
    }
    handle.env("RATTAN_ID", rattan_id.into());
}
