#[cfg(feature = "http")]
use std::net::IpAddr;
use std::{
    sync::{atomic::AtomicUsize, mpsc, Arc},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use backon::{BlockingRetryable, ExponentialBuilder};
use nix::{
    sched::{sched_setaffinity, CpuSet},
    unistd::Pid,
};
use once_cell::sync::{Lazy, OnceCell};
use rattan_env::{env::standard::AfPacketDriver, InterfaceDriver, StdNetEnv};
use rattan_env::{
    env::{RattanEnv, RattanEnvConfig},
    netns::NetNsGuard,
    InterfaceBuildArtifact,
};
use rattan_log::{file_logging_thread, RattanLogOp, LOGGING_TX};
use tokio::{runtime::Runtime, time::Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, span, warn, Level};

use crate::{
    cells::{
        external::{get_clock_ns, VirtualEthernet, VirtualEthernetId},
        Cell, Packet,
    },
    config::{CellBuildConfig, RattanConfig},
    control::{RattanOp, RattanOpEndpoint, RattanOpResult},
    core::{CellFactory, RattanCore},
    error::Error,
};

#[cfg(feature = "http")]
use crate::{control::http::HttpControlEndpoint, error::HttpServerError};
#[cfg(feature = "http")]
use std::net::{Ipv4Addr, SocketAddr};

pub static INSTANCE_ID: OnceCell<String> = OnceCell::new();

pub static BASE_TS: Lazy<(i64, u64, tokio::time::Instant)> = Lazy::new(|| {
    // Internal use
    let machine_time = get_clock_ns();
    // Used as base timestamp in Packet Logs.
    let unix_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_micros();
    (machine_time, unix_time as u64, Instant::now())
});

// Upon worker thread startup, this counter is incremented. So that the working threads
// are assigned unique CPU affinity from the given user-specified CPU set.
static AFFINITY_COUNTER: AtomicUsize = AtomicUsize::new(0);

// Utility function to set CPU affinity for the current working thread.
// If anything failes, an error is printed, and the worker thread will continue.
fn set_cpu_affinity(available_cpus: &[u32]) {
    let index = AFFINITY_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if available_cpus.is_empty() {
        return;
    }
    let cpu = available_cpus[index % available_cpus.len()] as usize;

    let mut cpu_set = CpuSet::new();
    if cpu_set.set(cpu).is_err() {
        error!(
            target : "working_thread",
            "Failed to set CPU affinity: CPU {} is not available", cpu
        );
        return;
    }

    info!(
        target : "working_thread",
        "Trying to set CPU affinity for working thread to CPU {}", cpu
    );

    if let Err(e) = sched_setaffinity(
        Pid::from_raw(0), // current thread
        &cpu_set,
    ) {
        error!(
            target : "working_thread",
            "Failed to set CPU affinity to CPU {}: {}", cpu, e
        );
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PacketLogMode {
    CompactTCP,
    RawIP,
    RawTCP,
    #[cfg(feature = "drift-stat")]
    DriftStat,
}

pub static PKT_LOG_MODE: OnceCell<PacketLogMode> = OnceCell::new();

pub type TaskResult<R> = Result<R, Box<dyn std::error::Error + Send + Sync>>;

pub trait Task<R: Send>: FnOnce() -> TaskResult<R> + Send {}

impl<R: Send, T: FnOnce() -> TaskResult<R> + Send> Task<R> for T {}

pub enum TaskResultNotify {
    Left,
    Right,
}

// Manage environment and resources
#[derive(derive_more::Deref)]
pub struct RattanRadix<D, E>
where
    D: InterfaceDriver + Send,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
    E: RattanEnv<D>,
{
    #[deref]
    env: E,
    cancel_token: CancellationToken,
    rattan_thread_handle: Option<thread::JoinHandle<()>>, // Use option to allow take ownership in drop
    log_thread_handle: Option<thread::JoinHandle<std::io::Result<()>>>,
    _rattan_runtime: Arc<Runtime>,
    rattan: RattanCore<D>,
    #[cfg(feature = "http")]
    http_thread_handle: Option<thread::JoinHandle<crate::error::Result<()>>>,
}

impl<D, E> RattanRadix<D, E>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
    E: RattanEnv<D>,
{
    pub fn new<EC>(config: RattanConfig<D::Packet, EC>) -> crate::error::Result<Self>
    where
        EC: RattanEnvConfig<BuildOutput = E>,
    {
        let instance_id = INSTANCE_ID.get_or_init(|| {
            // get env var from RATTAN_INSTANCE_ID
            std::env::var("RATTAN_INSTANCE_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
        });
        info!("New RattanRadix with instance id: {}", instance_id);
        let build_env = || {
            config.env.build().map_err(|e| {
                warn!("Failed to build environment, retrying");
                e.into()
            })
        };

        let env = build_env
            .retry(
                ExponentialBuilder::default()
                    .with_jitter()
                    .with_max_times(3),
            )
            .call()?;

        let running_core = config.resource.get_cpu().unwrap_or_default();
        let worker_thread_cnt = config.resource.working_threads().max(1);

        if running_core.is_empty() {
            info!(target : "working_thread", "Try to start {} working threads without setting CPU affinity", worker_thread_cnt);
        } else {
            info!(target : "working_thread", "Try to start {} working threads with CPU affinity on CPUs: {:?}",  worker_thread_cnt, running_core);
        };

        // Log env creation to kmsg for system-level observability
        if let Ok(mut kmesg_logger) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
            use std::io::Write;
            let mut buf = Vec::new();
            let _ = writeln!(
                buf,
                "rattan instance id {instance_id} create ns with rand_string {}",
                env.get_rattan_id()
            );
            let _ = kmesg_logger.write_all(&buf);
            let _ = kmesg_logger.flush();
        }
        let cancel_token = CancellationToken::new();

        let rattan_thread_span = span!(Level::ERROR, "rattan_thread").or_current();
        let rattan_ns = env.get_rattan_ns();
        let (runtime_tx, runtime_rx) = std::sync::mpsc::channel();
        let rt_cancel_token = CancellationToken::new();
        let rt_cancel_token_dup = rt_cancel_token.clone();
        let rattan_thread_handle = std::thread::spawn(move || {
            let _entered = rattan_thread_span.entered();
            info!("Rattan thread started");

            // Enter the network namespace specified by the environment
            if let Some(rattan_ns) = rattan_ns {
                if let Err(e) = rattan_ns.enter() {
                    error!("Failed to enter rattan namespace: {:?}", e);
                    runtime_tx.send(Err(e.into())).unwrap();
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10)); // BUG: sleep between namespace enter and runtime build

            // TODO(enhancement): need to handle panic due to affinity setting
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(worker_thread_cnt)
                .enable_all()
                .on_thread_start(move || {
                    set_cpu_affinity(running_core.as_slice());
                })
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

        let mode = config
            .general
            .packet_log_mode
            .unwrap_or(PacketLogMode::CompactTCP);
        PKT_LOG_MODE.set(mode).ok();

        let packet_log_path = config.general.packet_log;

        let log_thread_handle = packet_log_path.map(file_logging_thread);

        let mut radix = Self {
            env,
            cancel_token: cancel_token.clone(),
            rattan_thread_handle: Some(rattan_thread_handle),
            log_thread_handle,
            _rattan_runtime: rattan_runtime,
            rattan,
            #[cfg(feature = "http")]
            http_thread_handle,
        };
        // This could be veth pairs, or rvnic
        radix
            .init_iterfaces()
            .inspect_err(|e| tracing::error!(?e, "Failed to init interfaces"))?;
        radix
            .load_cells_config(config.cells)
            .inspect_err(|e| tracing::error!(?e, "Failed to load cell config"))?;
        radix
            .link_cells(config.links)
            .inspect_err(|e| tracing::error!(?e, "Failed to link cells"))?;
        Ok(radix)
    }

    pub fn build_cell<V, F>(
        &mut self,
        id: String,
        builder: F,
    ) -> Result<Arc<V::ControlInterfaceType>, Error>
    where
        V: Cell<D::Packet>,
        F: CellFactory<V>,
    {
        self.rattan.build_cell(id, builder)
    }

    pub fn link_cell(&mut self, rx_id: String, tx_id: String) {
        self.rattan.link_cell(rx_id, tx_id);
    }

    pub fn init_interface(&mut self, interface: InterfaceBuildArtifact<D>) -> Result<(), Error> {
        let InterfaceBuildArtifact {
            ns_id,
            veth_id,
            name,
            drivers,
        } = interface;

        let name_clone = name.clone();

        self.build_cell(name.clone(), move |rt| {
            let _guard = rt.enter();
            let mut id = VirtualEthernetId::new();
            id.set_ns_id(ns_id);
            id.set_veth_id_copied(veth_id);
            VirtualEthernet::<D>::new(drivers, id)
        })
        .inspect_err(|e| tracing::error!(?e, "Failed to init interface {}", name_clone))?;

        Ok(())
    }

    pub fn init_iterfaces(&mut self) -> Result<(), Error> {
        // If the interfaces need sepcified netns to be built in, it is the Env's
        // responsibility to do so in `build_interfaces`.
        let interfaces = self
            .env
            .build_interfaces(self.rattan.get_runtime_handle())?;
        info!("{} interfaces to be registered as cells.", interfaces.len());
        for interface in interfaces.into_iter() {
            self.init_interface(interface)?;
        }
        info!("Interfaces registered as cells");
        Ok(())
    }

    pub fn load_cells_config<I>(&mut self, cells: I) -> Result<(), Error>
    where
        I: IntoIterator<Item = (String, CellBuildConfig<D::Packet>)>,
    {
        let mut router_configs = vec![];

        // build cells, EXCEPT routers
        for (id, cell_config) in cells {
            match cell_config {
                CellBuildConfig::Bw(bw_config) => match bw_config {
                    crate::config::BwCellBuildConfig::Infinite(config) => {
                        self.build_cell(id, config.into_factory())?;
                    }
                    crate::config::BwCellBuildConfig::DropTail(config) => {
                        self.build_cell(id, config.into_factory())?;
                    }
                    crate::config::BwCellBuildConfig::DropHead(config) => {
                        self.build_cell(id, config.into_factory())?;
                    }
                    crate::config::BwCellBuildConfig::CoDel(config) => {
                        self.build_cell(id, config.into_factory())?;
                    }
                },
                CellBuildConfig::BwReplay(bw_replay_config) => match bw_replay_config {
                    crate::config::BwReplayCellBuildConfig::Infinite(config) => {
                        self.build_cell(id, config.into_factory())?;
                    }
                    crate::config::BwReplayCellBuildConfig::DropTail(config) => {
                        self.build_cell(id, config.into_factory())?;
                    }
                    crate::config::BwReplayCellBuildConfig::DropHead(config) => {
                        self.build_cell(id, config.into_factory())?;
                    }
                    crate::config::BwReplayCellBuildConfig::CoDel(config) => {
                        self.build_cell(id, config.into_factory())?;
                    }
                },
                CellBuildConfig::Delay(config) => {
                    self.build_cell(id, config.into_factory())?;
                }
                CellBuildConfig::DelayReplay(config) => {
                    self.build_cell(id, config.into_factory())?;
                }
                CellBuildConfig::Loss(config) => {
                    self.build_cell(id, config.into_factory())?;
                }
                CellBuildConfig::LossReplay(config) => {
                    self.build_cell(id, config.into_factory())?;
                }
                CellBuildConfig::Shadow(config) => {
                    self.build_cell(id, config.into_factory())?;
                }
                CellBuildConfig::Spy(config) => {
                    self.build_cell(id, config.into_factory())?;
                }
                CellBuildConfig::DelayPerPacket(config) => {
                    self.build_cell(id, config.into_factory())?;
                }
                CellBuildConfig::Router(config) => {
                    // ignore routers, build them after other cells
                    router_configs.push((id, config));
                }
                CellBuildConfig::TokenBucket(config) => {
                    self.build_cell(id, config.into_factory())?;
                }
                CellBuildConfig::Custom => {
                    debug!("Skip build custom cell: {}", id);
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
            self.build_cell(id, config.into_factory(receivers))?;
        }

        Ok(())
    }

    pub fn link_cells<I>(&mut self, links: I) -> Result<(), Error>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        for (rx, tx) in links {
            self.link_cell(rx, tx);
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

    // Spawn a thread running task in left namespace
    pub fn left_spawn<R: Send + 'static>(
        &self,
        tx: Option<mpsc::Sender<TaskResultNotify>>,
        task: impl Task<R> + 'static,
    ) -> Result<thread::JoinHandle<TaskResult<R>>, Error> {
        let thread_span = span!(Level::INFO, "left_ns").or_current();
        let left_ns = self.env.get_left_ns();
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
        let right_ns = self.env.get_right_ns();
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
}

impl RattanRadix<AfPacketDriver, StdNetEnv> {
    // ping specialized right veth from left NS through specialized left veth
    pub fn ping_right_from_left(
        &self,
        left_pair_id: usize,
        right_pair_id: usize,
    ) -> Result<bool, Error> {
        let src_ip = <StdNetEnv as RattanEnv<AfPacketDriver>>::left_ip(self, left_pair_id);
        let dest_ip = <StdNetEnv as RattanEnv<AfPacketDriver>>::right_ip(self, right_pair_id);
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
                &self.env.left_pairs.get(&left_pair_id).unwrap().left.name,
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let output = handle.wait_with_output()?;
        let stdout = String::from_utf8(output.stdout).unwrap();
        info!("ping output: {}", stdout);
        Ok(stdout.contains("time="))
    }
}

impl<D, E> Drop for RattanRadix<D, E>
where
    D: InterfaceDriver + Send,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
    E: RattanEnv<D>,
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
                if let Err(e) = log_thread_handle.join().unwrap() {
                    tracing::error!("Error from logging thread {:?}", e);
                }
            }
        }
        info!("RattanRadix dropped");
    }
}
