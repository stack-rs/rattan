use std::{net::IpAddr, sync::Arc, thread};

use backon::{BlockingRetryable, ExponentialBuilder};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, span, warn, Level};

use crate::{
    config::{DeviceBuildConfig, RattanConfig, RattanCoreConfig},
    control::{RattanOp, RattanOpEndpoint, RattanOpResult},
    core::{DeviceFactory, RattanCore},
    devices::{external::VirtualEthernet, Device, Packet},
    env::{get_std_env, StdNetEnv},
    error::Error,
    metal::{io::AfPacketDriver, netns::NetNsGuard},
};

#[cfg(feature = "http")]
use crate::control::http::HttpControlEndpoint;
#[cfg(feature = "http")]
use std::net::{Ipv4Addr, SocketAddr};

pub trait Task<R: Send>: FnOnce() -> anyhow::Result<R> + Send {}

impl<R: Send, T: FnOnce() -> anyhow::Result<R> + Send> Task<R> for T {}

// Manage environment and resources
pub struct RattanRadix<P>
where
    P: Packet + Sync,
{
    env: StdNetEnv,
    cancel_token: CancellationToken,

    rattan_thread_handle: Option<thread::JoinHandle<()>>, // Use option to allow take ownership in drop
    _rattan_runtime: Arc<Runtime>,
    rattan: RattanCore<P>,
    #[cfg(feature = "http")]
    http_thread_handle: Option<thread::JoinHandle<anyhow::Result<()>>>,
}

impl<P> RattanRadix<P>
where
    P: Packet + Sync,
{
    pub fn new(config: RattanConfig<P>) -> Result<Self, Error> {
        info!("New RattanRadix");
        let build_env = || {
            get_std_env(&config.env).map_err(|e| {
                warn!("Failed to build environment, retrying");
                e
            })
        };
        let env = build_env
            .retry(
                &ExponentialBuilder::default()
                    .with_jitter()
                    .with_max_times(3),
            )
            .call()?;
        let cancel_token = CancellationToken::new();

        let rattan_thread_span = span!(Level::ERROR, "rattan_thread").or_current();
        let rattan_ns = env.rattan_ns.clone();
        let (runtime_tx, runtime_rx) = std::sync::mpsc::channel();
        let rt_cancel_token = CancellationToken::new();
        let rt_cancel_token_dup = rt_cancel_token.clone();
        let rattan_thread_handle = std::thread::spawn(move || {
            let _entered = rattan_thread_span.entered();
            info!("Rattan thread started");
            if let Err(e) = rattan_ns.enter() {
                error!("Failed to enter rattan namespace: {:?}", e);
                runtime_tx.send(Err(e.into())).unwrap();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map(Arc::new);
            match runtime {
                Ok(runtime) => {
                    runtime_tx.send(Ok(runtime.clone())).unwrap();
                    runtime.block_on(rt_cancel_token_dup.cancelled());
                }
                Err(e) => {
                    error!("Failed to build runtime: {:?}", e);
                    runtime_tx
                        .send(Err(Error::TokioRuntimeError(e.into())))
                        .unwrap();
                }
            }
            info!("Rattan thread exited");
        });
        let rattan_runtime = runtime_rx.recv().map_err(|e| {
            error!("Failed to get runtime handle: {:?}", e);
            Error::ChannelError(e.to_string())
        })??;
        let rattan = RattanCore::new(
            rattan_runtime.clone(),
            cancel_token.child_token(),
            rt_cancel_token,
        );

        #[cfg(feature = "http")]
        let http_thread_handle = if config.http.enable {
            let http_cancel_token = cancel_token.child_token();
            let op_endpoint = rattan.op_endpoint();
            let http_thread_span = span!(Level::INFO, "http_thread").or_current();
            Some(std::thread::spawn(move || -> anyhow::Result<()> {
                let _entered = http_thread_span.entered();
                info!("HTTP thread started");
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        error!("Failed to build http runtime: {:?}", e);
                        e
                    })?;
                runtime.block_on(async move {
                    let address =
                        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), config.http.port);
                    let server = axum::Server::bind(&address)
                        .serve(
                            HttpControlEndpoint::new(op_endpoint)
                                .router()
                                .into_make_service(),
                        )
                        .with_graceful_shutdown(async {
                            http_cancel_token.cancelled().await;
                        });
                    info!("HTTP server listening on http://{}", address);
                    server.await.map_err(|e| {
                        error!("HTTP Server error: {}", e);
                        e
                    })
                })?;
                info!("HTTP thread exited");
                Ok(())
            }))
        } else {
            info!("HTTP server disabled");
            None
        };

        let mut radix = Self {
            env,
            cancel_token: cancel_token.clone(),
            rattan_thread_handle: Some(rattan_thread_handle),
            _rattan_runtime: rattan_runtime,
            rattan,
            #[cfg(feature = "http")]
            http_thread_handle,
        };
        radix.init_veth()?; // build veth pair at the beginning
        radix.load_core_config(config.core)?;
        Ok(radix)
    }

    pub fn build_deivce<D, F>(
        &mut self,
        id: String,
        builder: F,
    ) -> Result<Arc<D::ControlInterfaceType>, Error>
    where
        D: Device<P>,
        F: DeviceFactory<D>,
    {
        self.rattan.build_deivce(id, builder)
    }

    pub fn link_device(&mut self, rx_id: String, tx_id: String) {
        self.rattan.link_device(rx_id, tx_id);
    }

    pub fn init_veth(&mut self) -> Result<(), Error> {
        let rattan_ns = self.env.rattan_ns.clone();
        let veth = self.env.left_pair.right.clone();
        self.build_deivce("left".to_string(), move |rt| {
            let _guard = rt.enter();
            let _ns_guard = NetNsGuard::new(rattan_ns);
            VirtualEthernet::<P, AfPacketDriver>::new(veth, "left".to_string())
        })?;

        let rattan_ns = self.env.rattan_ns.clone();
        let veth = self.env.right_pair.left.clone();
        self.build_deivce("right".to_string(), move |rt| {
            let _guard = rt.enter();
            let _ns_guard = NetNsGuard::new(rattan_ns);
            VirtualEthernet::<P, AfPacketDriver>::new(veth, "right".to_string())
        })?;

        Ok(())
    }

    pub fn load_core_config(&mut self, config: RattanCoreConfig<P>) -> Result<(), Error> {
        // build devices
        for (id, device_config) in config.devices {
            match device_config {
                DeviceBuildConfig::Bw(bw_config) => match bw_config {
                    crate::config::BwDeviceBuildConfig::Infinite(config) => {
                        self.build_deivce(id, config.into_factory())?;
                    }
                    crate::config::BwDeviceBuildConfig::DropTail(config) => {
                        self.build_deivce(id, config.into_factory())?;
                    }
                    crate::config::BwDeviceBuildConfig::DropHead(config) => {
                        self.build_deivce(id, config.into_factory())?;
                    }
                    crate::config::BwDeviceBuildConfig::CoDel(config) => {
                        self.build_deivce(id, config.into_factory())?;
                    }
                },
                DeviceBuildConfig::BwReplay(bw_replay_config) => match bw_replay_config {
                    crate::config::BwReplayDeviceBuildConfig::Infinite(config) => {
                        self.build_deivce(id, config.into_factory())?;
                    }
                    crate::config::BwReplayDeviceBuildConfig::DropTail(config) => {
                        self.build_deivce(id, config.into_factory())?;
                    }
                    crate::config::BwReplayDeviceBuildConfig::DropHead(config) => {
                        self.build_deivce(id, config.into_factory())?;
                    }
                    crate::config::BwReplayDeviceBuildConfig::CoDel(config) => {
                        self.build_deivce(id, config.into_factory())?;
                    }
                },
                DeviceBuildConfig::Delay(config) => {
                    self.build_deivce(id, config.into_factory())?;
                }
                DeviceBuildConfig::Loss(config) => {
                    self.build_deivce(id, config.into_factory())?;
                }
                DeviceBuildConfig::Custom => {
                    debug!("Skip build custom device: {}", id);
                }
            }
        }

        // link devices
        for (rx, tx) in config.links {
            self.link_device(rx, tx);
        }

        Ok(())
    }

    pub fn spawn_rattan(&mut self) -> Result<(), Error> {
        self.rattan.spawn_rattan().map_err(|e| e.into())
    }

    pub fn start_rattan(&mut self) -> Result<(), Error> {
        self.rattan.start_rattan()
    }

    pub fn join_rattan(&mut self) {
        self.rattan.join_rattan()
    }

    pub fn cancel_rattan(&mut self) {
        self.rattan.cancel_rattan()
    }

    pub fn op_endpoint(&self) -> RattanOpEndpoint {
        self.rattan.op_endpoint()
    }

    pub fn op_block_exec(&self, op: RattanOp) -> Result<RattanOpResult, Error> {
        self.rattan.op_block_exec(op)
    }

    pub fn left_ip(&self) -> IpAddr {
        self.env.left_pair.left.ip_addr.0
    }

    pub fn right_ip(&self) -> IpAddr {
        self.env.right_pair.right.ip_addr.0
    }

    // Spawn a thread running task in left namespace
    pub fn left_spawn<R: Send + 'static>(
        &self,
        task: impl Task<R> + 'static,
    ) -> Result<thread::JoinHandle<anyhow::Result<R>>, Error> {
        let thread_span = span!(Level::INFO, "left_ns").or_current();
        let left_ns = self.env.left_ns.clone();
        Ok(std::thread::spawn(move || {
            let _entered = thread_span.entered();
            if let Err(e) = left_ns.enter() {
                error!("Failed to enter left namespace: {:?}", e);
                return anyhow::Result::Err(e.into());
            }
            info!("Run task in left namespace");
            task()
        }))
    }

    // Spawn a thread running task in right namespace
    pub fn right_spawn<R: Send + 'static>(
        &self,
        task: impl Task<R> + 'static,
    ) -> Result<thread::JoinHandle<anyhow::Result<R>>, Error> {
        let thread_span = span!(Level::INFO, "right_ns").or_current();
        let right_ns = self.env.right_ns.clone();
        Ok(std::thread::spawn(move || {
            let _entered = thread_span.entered();
            if let Err(e) = right_ns.enter() {
                error!("Failed to enter right namespace: {:?}", e);
                return anyhow::Result::Err(e.into());
            }
            info!("Run task in right namespace");
            task()
        }))
    }

    pub fn ping_test(&self) -> Result<bool, Error> {
        info!("ping {} testing...", self.right_ip());
        let _left_ns_guard = NetNsGuard::new(self.env.left_ns.clone())?;
        let handle = std::process::Command::new("ping")
            .args([&self.right_ip().to_string(), "-c", "3", "-i", "0.2"])
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let output = handle.wait_with_output()?;
        let stdout = String::from_utf8(output.stdout).unwrap();
        debug!("ping output: {}", stdout);
        Ok(stdout.contains("time="))
    }
}

impl<P> Drop for RattanRadix<P>
where
    P: Packet + Sync,
{
    fn drop(&mut self) {
        debug!("Cancelling RattanRadix");
        self.cancel_token.cancel();
        #[cfg(feature = "http")]
        {
            debug!("Wait for http thread to finish");
            if let Some(http_thread_handle) = self.http_thread_handle.take() {
                if http_thread_handle.join().unwrap().is_err() {
                    error!("HTTP thread exited due to error");
                }
            }
        }
        debug!("Wait for rattan cancellation");
        self.cancel_rattan();
        debug!("Wait for rattan thread to finish");
        if let Some(rattan_thread_handle) = self.rattan_thread_handle.take() {
            rattan_thread_handle.join().unwrap();
        }
        info!("RattanRadix dropped");
    }
}
