use std::{process::Stdio, sync::Arc};

use bollard::{
    exec::{CreateExecOptions, StartExecResults},
    Docker,
};
use futures::TryStreamExt;
use rand::{rngs::StdRng, SeedableRng};
use rattan::{
    core::{RattanMachine, RattanMachineConfig},
    devices::{
        bandwidth::{queue::InfiniteQueue, BwDevice, BwDeviceConfig, MAX_BANDWIDTH},
        external::VirtualEthernet,
    },
    env::get_container_env,
    metal::{io::AfPacketDriver, netns::NetNs},
};
use rattan::{
    devices::{
        delay::{DelayDevice, DelayDeviceConfig},
        loss::{IIDLossDevice, IIDLossDeviceConfig},
        ControlInterface, Device, StdPacket,
    },
    netem_trace::Bandwidth,
};
use tracing::{debug, info, instrument, span, trace, warn, Instrument, Level};

use crate::CommandArgs;

#[instrument(skip_all, level = "debug")]
async fn docker_exec(docker: &Docker, id: &str, cmd: Vec<&str>) -> anyhow::Result<()> {
    debug!(?id, ?cmd, "Docker exec start");
    let exec = loop {
        match docker
            .create_exec(
                id,
                CreateExecOptions {
                    cmd: Some(cmd.clone()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(exec) => break exec.id,
            Err(e) => {
                warn!(?e, "Docker exec error, retrying...");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        }
    };
    if let StartExecResults::Attached { mut output, .. } = docker.start_exec(&exec, None).await? {
        while let Some(msg) = output.try_next().await? {
            debug!(?msg, "Docker exec output");
        }
    } else {
        unreachable!("This docker exec is not attached");
    }
    debug!("Docker exec end");
    Ok(())
}

#[instrument(skip_all, level = "debug")]
async fn start_handler(docker: Docker, id: String) -> anyhow::Result<()> {
    let container = docker.inspect_container(&id, None).await?;
    for env in container.config.unwrap().env.unwrap() {
        if env.starts_with("RATTAN_HOST=") {
            let host = env.split('=').nth(1).unwrap();
            debug!(?host, "env RATTAN_HOST");
            docker_exec(&docker, &id, vec!["ip", "route", "del", "default"]).await?;
            docker_exec(
                &docker,
                &id,
                vec!["ip", "route", "add", "default", "via", host],
            )
            .await?;
        }
        if env.starts_with("RATTAN_HOST6=") {
            let host = env.split('=').nth(1).unwrap();
            debug!(?host, "env RATTAN_HOST6");
            docker_exec(&docker, &id, vec!["ip", "-6", "route", "del", "default"]).await?;
            docker_exec(
                &docker,
                &id,
                vec!["ip", "-6", "route", "add", "default", "via", host],
            )
            .await?;
        }
    }
    Ok(())
}

#[instrument(skip_all, level = "debug")]
async fn listen_docker_events() -> anyhow::Result<()> {
    let docker = Docker::connect_with_local_defaults()?;
    let mut events = docker.events::<String>(None);
    while let Some(event) = events.try_next().await? {
        if event.action == Some("start".to_string()) {
            trace!(?event, "Docker event [start]");
            let id = event.actor.unwrap().id.unwrap();
            debug!(?id, "Docker event [start]");
            tokio::spawn(start_handler(docker.clone(), id));
        }
    }
    Ok(())
}

#[instrument(skip_all, level = "debug")]
pub fn docker_main(opts: CommandArgs) -> anyhow::Result<()> {
    let loss = opts.loss;
    let delay = opts.delay;
    let bandwidth = opts.bandwidth.map(Bandwidth::from_bps);
    let container_env = get_container_env()?;

    let rt = tokio::runtime::Runtime::new()?;
    let listen_handle = rt.spawn(listen_docker_events());

    let mut machine = RattanMachine::<StdPacket>::new();
    let cancel_token = machine.cancel_token();

    if container_env.veth_list.len() != 2 {
        return Err(anyhow::anyhow!(
            "veth_list length is not 2, get {:?}",
            container_env.veth_list.len()
        ));
    }

    // Use iptables to drop all forward packets. Forward packets will only be processed by rattan with raw socket.
    let output = std::process::Command::new("iptables")
        .arg("-A")
        .arg("FORWARD")
        .arg("-j")
        .arg("DROP")
        .output()?;
    debug!(?output, "iptables -A FORWARD -j DROP",);
    let output = std::process::Command::new("iptables")
        .arg("-A")
        .arg("FORWARD")
        .arg("-j")
        .arg("DROP")
        .output()?;
    debug!(?output, "ip6tables -A FORWARD -j DROP",);

    let rattan_thread_span = span!(Level::DEBUG, "rattan_thread").or_current();
    let rattan_thread = std::thread::spawn(move || {
        let _entered = rattan_thread_span.entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(
            async move {
                let rng = StdRng::seed_from_u64(42);

                let left_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(Arc::new(
                    container_env.veth_list[0].clone(),
                ));
                let right_device = VirtualEthernet::<StdPacket, AfPacketDriver>::new(Arc::new(
                    container_env.veth_list[1].clone(),
                ));

                let (left_device_rx, left_device_tx) = machine.add_device(left_device);
                info!(left_device_rx, left_device_tx);
                let (right_device_rx, right_device_tx) = machine.add_device(right_device);
                info!(right_device_rx, right_device_tx);
                let mut left_fd = vec![left_device_rx];
                let mut right_fd = vec![right_device_rx];
                if let Some(bandwidth) = bandwidth {
                    let left_bw_device = BwDevice::new(MAX_BANDWIDTH, InfiniteQueue::new());
                    let right_bw_device = BwDevice::new(MAX_BANDWIDTH, InfiniteQueue::new());
                    let left_bw_ctl = left_bw_device.control_interface();
                    let right_bw_ctl = right_bw_device.control_interface();
                    left_bw_ctl
                        .set_config(BwDeviceConfig::new(bandwidth, None))
                        .unwrap();
                    right_bw_ctl
                        .set_config(BwDeviceConfig::new(bandwidth, None))
                        .unwrap();
                    let (left_bw_rx, left_bw_tx) = machine.add_device(left_bw_device);
                    info!(left_bw_rx, left_bw_tx);
                    let (right_bw_rx, right_bw_tx) = machine.add_device(right_bw_device);
                    info!(right_bw_rx, right_bw_tx);
                    left_fd.push(left_bw_tx);
                    left_fd.push(left_bw_rx);
                    right_fd.push(right_bw_tx);
                    right_fd.push(right_bw_rx);
                }
                if let Some(delay) = delay {
                    let left_delay_device = DelayDevice::<StdPacket>::new();
                    let right_delay_device = DelayDevice::<StdPacket>::new();
                    let left_delay_ctl = left_delay_device.control_interface();
                    let right_delay_ctl = right_delay_device.control_interface();
                    left_delay_ctl
                        .set_config(DelayDeviceConfig::new(delay))
                        .unwrap();
                    right_delay_ctl
                        .set_config(DelayDeviceConfig::new(delay))
                        .unwrap();
                    let (left_delay_rx, left_delay_tx) = machine.add_device(left_delay_device);
                    info!(left_delay_rx, left_delay_tx);
                    let (right_delay_rx, right_delay_tx) = machine.add_device(right_delay_device);
                    info!(right_delay_rx, right_delay_tx);
                    left_fd.push(left_delay_tx);
                    left_fd.push(left_delay_rx);
                    right_fd.push(right_delay_tx);
                    right_fd.push(right_delay_rx);
                }
                if let Some(loss) = loss {
                    let left_loss_device = IIDLossDevice::<StdPacket, StdRng>::new(rng.clone());
                    let right_loss_device = IIDLossDevice::<StdPacket, StdRng>::new(rng);
                    let right_loss_ctl = right_loss_device.control_interface();
                    right_loss_ctl
                        .set_config(IIDLossDeviceConfig::new(loss))
                        .unwrap();
                    let (left_loss_rx, left_loss_tx) = machine.add_device(left_loss_device);
                    info!(left_loss_rx, left_loss_tx);
                    let (right_loss_rx, right_loss_tx) = machine.add_device(right_loss_device);
                    info!(right_loss_rx, right_loss_tx);
                    left_fd.push(left_loss_tx);
                    left_fd.push(left_loss_rx);
                    right_fd.push(right_loss_tx);
                    right_fd.push(right_loss_rx);
                }

                left_fd.push(right_device_tx);
                if left_fd.len() % 2 != 0 {
                    panic!("Wrong number of devices");
                }
                for i in 0..left_fd.len() / 2 {
                    machine.link_device(left_fd[i * 2], left_fd[i * 2 + 1]);
                }
                right_fd.push(left_device_tx);
                if right_fd.len() % 2 != 0 {
                    panic!("Wrong number of devices");
                }
                for i in 0..right_fd.len() / 2 {
                    machine.link_device(right_fd[i * 2], right_fd[i * 2 + 1]);
                }

                let config = RattanMachineConfig {
                    original_ns: NetNs::current().unwrap(),
                    port: 8086,
                };
                machine.core_loop(config).await
            }
            .in_current_span(),
        );
    });

    let mut client_handle = std::process::Command::new("/bin/bash");
    if !opts.commands.is_empty() {
        client_handle.arg("-c").args(opts.commands);
    }
    let mut client_handle = client_handle
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let output = client_handle.wait().unwrap();
    info!("Exit {}", output.code().unwrap());

    cancel_token.cancel();
    listen_handle.abort();
    rattan_thread.join().unwrap();
    Ok(())
}
