use std::{
    io::Write,
    net::IpAddr,
    sync::{mpsc, Arc},
    thread,
};

use backon::{BlockingRetryable, ExponentialBuilder};
use once_cell::sync::OnceCell;
// use nix::{
//     sched::{sched_setaffinity, CpuSet},
//     unistd::Pid,
// };
use tokio::{runtime::Runtime, sync::mpsc::UnboundedSender};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, span, warn, Level};

use crate::{
    config::{DeviceBuildConfig, RattanConfig},
    control::{RattanOp, RattanOpEndpoint, RattanOpResult},
    core::{DeviceFactory, RattanCore},
    devices::{
        external::{VirtualEthernet, VirtualEthernetId},
        Device, Packet,
    },
    env::{get_std_env, StdNetEnv, StdNetEnvMode},
    error::Error,
    metal::{io::common::InterfaceDriver, netns::NetNsGuard},
};

#[cfg(feature = "http")]
use crate::{control::http::HttpControlEndpoint, error::HttpServerError};
#[cfg(feature = "http")]
use std::net::{Ipv4Addr, SocketAddr};

pub mod log;

pub use log::RattanLogOp;

pub static INSTANCE_ID: OnceCell<String> = OnceCell::new();
pub static LOGGING_TX: OnceCell<UnboundedSender<RattanLogOp>> = OnceCell::new();

pub type TaskResult<R> = Result<R, Box<dyn std::error::Error + Send + Sync>>;

pub trait Task<R: Send>: FnOnce() -> TaskResult<R> + Send {}

impl<R: Send, T: FnOnce() -> TaskResult<R> + Send> Task<R> for T {}

pub enum TaskResultNotify {
    Left,
    Right,
}

// Manage environment and resources
pub struct RattanRadix<D>
where
    D: InterfaceDriver + Send,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
{
    env: StdNetEnv,
    mode: StdNetEnvMode,
    cancel_token: CancellationToken,
    rattan_thread_handle: Option<thread::JoinHandle<()>>, // Use option to allow take ownership in drop
    log_thread_handle: Option<thread::JoinHandle<()>>,
    _rattan_runtime: Arc<Runtime>,
    rattan: RattanCore<D>,
    #[cfg(feature = "http")]
    http_thread_handle: Option<thread::JoinHandle<crate::error::Result<()>>>,
}

impl<D> RattanRadix<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
{
    pub fn new(config: RattanConfig<D::Packet>) -> crate::error::Result<Self> {
        let instance_id = INSTANCE_ID.get_or_init(|| {
            // get env var from RATTAN_INSTANCE_ID
            std::env::var("RATTAN_INSTANCE_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
        });
        info!("New RattanRadix with instance id: {}", instance_id);
        let build_env = || {
            get_std_env(&config.env).inspect_err(|_| {
                warn!("Failed to build environment, retrying");
            })
        };
        let running_core = config.resource.cpu.clone().unwrap_or(vec![1]);

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

            // TODO(enhancement): need to handle panic due to affinity setting
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(running_core.len())
                .enable_all()
                // .on_thread_start(move || {
                //     let mut cpuset = CpuSet::new();
                //     for core in running_core.iter() {
                //         cpuset.set(*core as usize).unwrap();
                //     }
                //     sched_setaffinity(Pid::from_raw(0), &cpuset).unwrap();
                // })
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
            let http_cancel_token = cancel_token.clone();
            let op_endpoint = rattan.op_endpoint();
            let http_thread_span = span!(Level::INFO, "http_thread").or_current();
            Some(std::thread::spawn(move || -> crate::error::Result<()> {
                let _entered = http_thread_span.entered();
                info!("HTTP thread started");
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        let err: Error = HttpServerError::TokioRuntimeError(e).into();
                        error!("{}", err);
                        http_cancel_token.cancel();
                        err
                    })?;
                let shutdown_cancel_token = http_cancel_token.clone();
                runtime
                    .block_on(async move {
                        let address = SocketAddr::new(
                            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                            config.http.port,
                        );
                        let listener = tokio::net::TcpListener::bind(&address)
                            .await
                            .map_err(HttpServerError::BindAddrError)?;
                        let server =
                            axum::serve(listener, HttpControlEndpoint::new(op_endpoint).router())
                                .with_graceful_shutdown(async move {
                                    shutdown_cancel_token.cancelled().await;
                                });
                        info!("HTTP server listening on http://{}", address);
                        server.await.map_err(HttpServerError::ServerError)
                    })
                    .inspect_err(|e| {
                        error!("{}", e);
                        http_cancel_token.cancel();
                    })?;
                info!("HTTP thread exited");
                Ok(())
            }))
        } else {
            info!("HTTP server disabled");
            None
        };
        let log_thread_handle = if let Some(path) = config.general.packet_log {
            let (log_tx, mut log_rx) = tokio::sync::mpsc::unbounded_channel();
            LOGGING_TX.set(log_tx).unwrap();
            Some(std::thread::spawn(move || {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&path)
                    .unwrap();
                let flow_map_file = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(format!("{}.flows", path.display()))
                    .unwrap();
                let mut file = std::io::BufWriter::new(file);
                let mut flow_map_file = std::io::BufWriter::new(flow_map_file);
                while let Some(entry) = log_rx.blocking_recv() {
                    match entry {
                        RattanLogOp::Entry(entry) => {
                            file.write_all(&entry).unwrap();
                        }
                        RattanLogOp::Flow(flow_id, base_ts, flow_desc) => {
                            let flow_entry = log::FlowEntry {
                                flow_id,
                                base_ts,
                                flow_desc,
                            };
                            #[cfg(feature = "serde")]
                            {
                                let _ = serde_json::to_writer(&mut flow_map_file, &flow_entry)
                                    .inspect_err(|e| {
                                        error!("Failed to write flow desc: {:?}", e);
                                    });
                            }
                            #[cfg(not(feature = "serde"))]
                            {
                                flow_map_file
                                    .write_fmt(format_args!("{:?}", flow_entry))
                                    .unwrap();
                            }
                            flow_map_file.write_all(b"\n").unwrap();
                        }
                        RattanLogOp::End => {
                            tracing::debug!("Logging thread exit");
                            file.flush().unwrap();
                            flow_map_file.flush().unwrap();
                            break;
                        }
                    }
                }
            }))
        } else {
            None
        };

        let mut radix = Self {
            env,
            mode: config.env.mode,
            cancel_token: cancel_token.clone(),
            rattan_thread_handle: Some(rattan_thread_handle),
            log_thread_handle,
            _rattan_runtime: rattan_runtime,
            rattan,
            #[cfg(feature = "http")]
            http_thread_handle,
        };
        radix.init_veth()?; // build veth pair at the beginning
        radix.load_devices_config(config.devices)?;
        radix.link_devices(config.links)?;
        Ok(radix)
    }

    pub fn build_device<V, F>(
        &mut self,
        id: String,
        builder: F,
    ) -> Result<Arc<V::ControlInterfaceType>, Error>
    where
        V: Device<D::Packet>,
        F: DeviceFactory<V>,
    {
        self.rattan.build_device(id, builder)
    }

    pub fn link_device(&mut self, rx_id: String, tx_id: String) {
        self.rattan.link_device(rx_id, tx_id);
    }

    pub fn init_veth(&mut self) -> Result<(), Error> {
        // Ignore veth0 for now
        for i in 1..self.env.left_pairs.len() {
            let rattan_ns = self.env.rattan_ns.clone();
            let veth = self.env.left_pairs[i].right.clone();
            let name = if self.env.left_pairs.len() == 2 {
                // if only one veth pair
                "left".to_string()
            } else {
                format!("left{i}")
            };
            self.build_device(name.clone(), move |rt| {
                let _guard = rt.enter();
                let _ns_guard = NetNsGuard::new(rattan_ns);
                let mut id = VirtualEthernetId::new();
                id.set_ns_id(1);
                id.set_veth_id(i as u8);
                VirtualEthernet::<D>::new(veth, id)
            })?;
        }

        for i in 1..self.env.right_pairs.len() {
            let rattan_ns = self.env.rattan_ns.clone();
            let veth = self.env.right_pairs[i].left.clone();
            let name = if self.env.right_pairs.len() == 2 {
                "right".to_string()
            } else {
                format!("right{i}")
            };
            self.build_device(name.clone(), move |rt| {
                let _guard = rt.enter();
                let _ns_guard = NetNsGuard::new(rattan_ns);
                let mut id = VirtualEthernetId::new();
                id.set_ns_id(2);
                id.set_veth_id(i as u8);
                VirtualEthernet::<D>::new(veth, id)
            })?;
        }

        Ok(())
    }

    pub fn load_devices_config<I>(&mut self, devices: I) -> Result<(), Error>
    where
        I: IntoIterator<Item = (String, DeviceBuildConfig<D::Packet>)>,
    {
        let mut router_configs = vec![];

        // build devices, EXCEPT routers
        for (id, device_config) in devices {
            match device_config {
                DeviceBuildConfig::Bw(bw_config) => match bw_config {
                    crate::config::BwDeviceBuildConfig::Infinite(config) => {
                        self.build_device(id, config.into_factory())?;
                    }
                    crate::config::BwDeviceBuildConfig::DropTail(config) => {
                        self.build_device(id, config.into_factory())?;
                    }
                    crate::config::BwDeviceBuildConfig::DropHead(config) => {
                        self.build_device(id, config.into_factory())?;
                    }
                    crate::config::BwDeviceBuildConfig::CoDel(config) => {
                        self.build_device(id, config.into_factory())?;
                    }
                    crate::config::BwDeviceBuildConfig::DualPI2(config) => {
                        self.build_device(id, config.into_factory())?;
                    }
                },
                DeviceBuildConfig::BwReplay(bw_replay_config) => match bw_replay_config {
                    crate::config::BwReplayDeviceBuildConfig::Infinite(config) => {
                        self.build_device(id, config.into_factory())?;
                    }
                    crate::config::BwReplayDeviceBuildConfig::DropTail(config) => {
                        self.build_device(id, config.into_factory())?;
                    }
                    crate::config::BwReplayDeviceBuildConfig::DropHead(config) => {
                        self.build_device(id, config.into_factory())?;
                    }
                    crate::config::BwReplayDeviceBuildConfig::CoDel(config) => {
                        self.build_device(id, config.into_factory())?;
                    }
                    crate::config::BwReplayDeviceBuildConfig::DualPI2(config) => {
                        self.build_device(id, config.into_factory())?;
                    }
                },
                DeviceBuildConfig::Delay(config) => {
                    self.build_device(id, config.into_factory())?;
                }
                DeviceBuildConfig::DelayReplay(config) => {
                    self.build_device(id, config.into_factory())?;
                }
                DeviceBuildConfig::Loss(config) => {
                    self.build_device(id, config.into_factory())?;
                }
                DeviceBuildConfig::LossReplay(config) => {
                    self.build_device(id, config.into_factory())?;
                }
                DeviceBuildConfig::Shadow(config) => {
                    self.build_device(id, config.into_factory())?;
                }
                DeviceBuildConfig::Router(config) => {
                    // ignore routers, build them after other devices
                    router_configs.push((id, config));
                }
                DeviceBuildConfig::Custom => {
                    debug!("Skip build custom device: {}", id);
                }
            }
        }

        // build routers
        for (id, config) in router_configs {
            let receivers = config
                .egress_connections
                .iter()
                .map(|id| self.rattan.get_receiver(id))
                .collect::<Result<_, _>>()?;
            self.build_device(id, config.into_factory(receivers))?;
        }

        Ok(())
    }

    pub fn link_devices<I>(&mut self, links: I) -> Result<(), Error>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        for (rx, tx) in links {
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

    /// IP of i-th veth pair of `ns-left`
    ///
    /// 0 is for external connection
    pub fn left_ip(&self, i: usize) -> IpAddr {
        self.env.left_pairs[i].left.ip_addr.0
    }

    /// IP of i-th veth pair of `ns-right`
    ///
    /// 0 is for external connection
    pub fn right_ip(&self, i: usize) -> IpAddr {
        self.env.right_pairs[i].right.ip_addr.0
    }

    /// IP list of veth pairs of `ns-left`
    pub fn left_ip_list(&self) -> Vec<IpAddr> {
        self.env
            .left_pairs
            .iter()
            .map(|e| e.left.ip_addr.0)
            .collect()
    }

    /// IP list of veth pairs of `ns-right`
    pub fn right_ip_list(&self) -> Vec<IpAddr> {
        self.env
            .right_pairs
            .iter()
            .map(|e| e.right.ip_addr.0)
            .collect()
    }

    pub fn get_mode(&self) -> StdNetEnvMode {
        self.mode
    }

    // Spawn a thread running task in left namespace
    pub fn left_spawn<R: Send + 'static>(
        &self,
        tx: Option<mpsc::Sender<TaskResultNotify>>,
        task: impl Task<R> + 'static,
    ) -> Result<thread::JoinHandle<TaskResult<R>>, Error> {
        let thread_span = span!(Level::INFO, "left_ns").or_current();
        let left_ns = self.env.left_ns.clone();
        Ok(std::thread::spawn(move || {
            let _entered = thread_span.entered();
            left_ns.enter().map_err(|e| {
                error!("Failed to enter left namespace");
                let e: Error = e.into();
                e
            })?;
            std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and process spawn
            info!("Run task in left namespace");
            let res = task();
            if let Some(tx) = tx {
                let _ = tx.send(TaskResultNotify::Left);
            }
            res
        }))
    }

    // Spawn a thread running task in right namespace
    pub fn right_spawn<R: Send + 'static>(
        &self,
        tx: Option<mpsc::Sender<TaskResultNotify>>,
        task: impl Task<R> + 'static,
    ) -> Result<thread::JoinHandle<TaskResult<R>>, Error> {
        let thread_span = span!(Level::INFO, "right_ns").or_current();
        let right_ns = self.env.right_ns.clone();
        Ok(std::thread::spawn(move || {
            let _entered = thread_span.entered();
            right_ns.enter().map_err(|e| {
                error!("Failed to enter right namespace");
                let e: Error = e.into();
                e
            })?;
            std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and process spawn
            info!("Run task in right namespace");
            let res = task();
            if let Some(tx) = tx {
                let _ = tx.send(TaskResultNotify::Right);
            }
            res
        }))
    }

    // ping specialized right veth from left NS through specialized left veth
    pub fn ping_right_from_left(
        &self,
        left_pair_id: usize,
        right_pair_id: usize,
    ) -> Result<bool, Error> {
        let src_ip = self.left_ip(left_pair_id);
        let dest_ip = self.right_ip(right_pair_id);
        info!("Ping testing {} from {} ...", dest_ip, src_ip);

        let _left_ns_guard = NetNsGuard::new(self.env.left_ns.clone())?;
        let handle = std::process::Command::new("ping")
            .args([
                &dest_ip.to_string(),
                "-c",
                "3",
                "-i",
                "0.2",
                "-I",
                &self.env.left_pairs[left_pair_id].left.name,
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let output = handle.wait_with_output()?;
        let stdout = String::from_utf8(output.stdout).unwrap();
        info!("ping output: {}", stdout);
        Ok(stdout.contains("time="))
    }
}

impl<D> Drop for RattanRadix<D>
where
    D: InterfaceDriver + Send,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
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
        if let Some(log_thread_handle) = self.log_thread_handle.take() {
            if let Some(tx) = LOGGING_TX.get() {
                tx.send(RattanLogOp::End).unwrap();
                log_thread_handle.join().unwrap();
            }
        }
        info!("RattanRadix dropped");
    }
}
