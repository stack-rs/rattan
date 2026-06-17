use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use rand::distr::Alphanumeric;
use rand::{rng, RngExt};
use rvnic::{DropRing, RvnicDevice, RxRing, TxRing, Umem, UmemBuilder};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::sync::SetError;
use tokio::sync::{
    mpsc::{self},
    OnceCell,
};
use tracing::{info, instrument, warn};

use super::constants::*;
use super::utils::*;
use super::DriverMetaData;
use crate::env::rvnic::RvnicDriver;
use crate::env::{get_addresses_in_use, IpAddrLock};
use crate::error::MetalError;
use crate::{Error, InterfaceBuildArtifact, NetNs, RattanEnv, RattanEnvConfig, StdNetEnvMode};

lazy_static::lazy_static! {
    static ref RVNIC_ENV_LOCK: Arc<parking_lot::Mutex<()>> = Arc::new(parking_lot::Mutex::new(()));
}

pub type RvnicEnvMode = StdNetEnvMode;

// Packets that are meant to be dropped by rattan are sent to this channel.
pub static RVNIC_PACKET_RECYCLE_TX: OnceCell<mpsc::UnboundedSender<(QueueID, u64)>> =
    OnceCell::const_new();

pub static UMEM: OnceCell<Arc<Umem>> = OnceCell::const_new();

struct RvnicPacketRecycler {
    drop_rings: HashMap<QueueID, (DropRing, Vec<u64>)>,
    rx: mpsc::UnboundedReceiver<(QueueID, u64)>,
}

impl RvnicPacketRecycler {
    async fn run(mut self) {
        info!(target = "rvnic", "Packet Recycler start");

        while let Some((queue_id, token)) = self.rx.recv().await {
            tracing::debug!(
                target = "rvnic",
                "Token {} on queue {:?} to be recycled",
                token,
                queue_id
            );
            if let Some((drop_ring, buffer)) = self.drop_rings.get_mut(&queue_id) {
                buffer.push(token);
                let original_len = buffer.len();
                if original_len < BATCH_SIZE {
                    continue;
                }
                // We do not care the order that the tokens are recycled, so we push from the rear
                // of the buffer, and also send a send-able part from back
                let available = drop_ring.available() as usize;
                let to_send = available.min(BATCH_SIZE).min(original_len);
                if to_send == 0 {
                    continue;
                }
                let new_length = original_len - to_send;
                let (_remain, to_send_buffer) = buffer.as_slice().split_at(new_length);

                let actual_sent = drop_ring.produce(to_send_buffer);
                if actual_sent == to_send {
                    // This is the common case without any memory allocation
                    buffer.truncate(new_length);
                } else {
                    // It is new_length..(new_length + actual_sent) that is sent,
                    // we have to remove them from the buffer
                    let not_sent_tail = buffer.split_off(new_length + actual_sent);
                    buffer.truncate(new_length);
                    buffer.extend_from_slice(&not_sent_tail);
                }

                tracing::debug!(
                    target = "rvnic",
                    "On {:?}, tried to drop {} tokens, dropped {}, remain {}",
                    queue_id,
                    to_send,
                    actual_sent,
                    buffer.len()
                );
            }
        }
    }
}
#[derive(PartialEq, Eq, Clone, Copy, Debug, Hash)]
pub struct QueueID {
    pub device_fd: i32,
    pub queue_id: u32,
}

impl std::fmt::Display for QueueID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("d")?;
        self.device_fd.fmt(f)?;
        f.write_str("q")?;
        self.queue_id.fmt(f)
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone)]
pub struct RvnicEnvConfig {
    #[cfg_attr(feature = "serde", serde(default))]
    pub mode: RvnicEnvMode,

    #[cfg_attr(feature = "serde", serde(default = "default_num_queues"))]
    pub num_queues: usize,

    // These settings are not supported for RvnicEnv now
    // #[cfg_attr(feature = "serde", serde(default = "default_veth_count"))]
    // pub left_veth_count: usize,
    // #[cfg_attr(feature = "serde", serde(default = "default_veth_count"))]
    // pub right_veth_count: usize,

    // #[cfg_attr(feature = "serde", serde(default))]
    // pub left_external: Option<NetDevice>,

    // #[cfg_attr(feature = "serde", serde(default))]
    // pub right_external: Option<NetDevice>,

    // This should be consistent with the kernel module!

    // TODO(minhuw): pretty sure these two configs should not be here
    // but let it be for now
    #[cfg_attr(feature = "serde", serde(default))]
    pub server_cores: Vec<usize>,

    #[cfg_attr(feature = "serde", serde(default))]
    pub client_cores: Vec<usize>,

    /// Rvnic uses a separate recv core for each end. This can be overwritten in the config.
    #[cfg_attr(feature = "serde", serde(default = "default_left_recv_core"))]
    pub left_recv_core: u32,
    /// Rvnic uses a separate recv core for each end. This can be overwritten in the config.
    #[cfg_attr(feature = "serde", serde(default = "default_right_recv_core"))]
    pub right_recv_core: u32,
    /// Core to do NAPI polling on the left rvnic. It can be -1 to use the core that kicked the rx. This can be overritten in the config.
    #[cfg_attr(feature = "serde", serde(default = "default_left_napi_core"))]
    pub left_napi_core: i32,
    /// Core to do NAPI polling on the right rvnic. It can be -1 to use the core that kicked the rx.
    #[cfg_attr(feature = "serde", serde(default = "default_right_napi_core"))]
    pub right_napi_core: i32,
}

impl Default for RvnicEnvConfig {
    fn default() -> Self {
        Self {
            mode: RvnicEnvMode::default(),
            num_queues: default_num_queues(),
            server_cores: Vec::new(),
            client_cores: Vec::new(),
            left_recv_core: default_left_recv_core(),
            right_recv_core: default_right_recv_core(),
            left_napi_core: default_left_napi_core(),
            right_napi_core: default_right_napi_core(),
        }
    }
}

impl RattanEnvConfig for RvnicEnvConfig {
    type BuildError = crate::error::Error;
    type Driver = RvnicDriver;
    type BuildOutput = RvnicEnv;

    fn build(&self) -> Result<Self::BuildOutput, Self::BuildError> {
        get_rvnic_env(self)
    }
    fn default_with_mode(mode: <Self::BuildOutput as RattanEnv<Self::Driver>>::Mode) -> Self {
        RvnicEnvConfig {
            mode,
            num_queues: default_num_queues(),
            server_cores: vec![1],
            client_cores: vec![3],
            left_recv_core: default_left_recv_core(),
            right_recv_core: default_right_recv_core(),
            left_napi_core: default_left_napi_core(),
            right_napi_core: default_right_napi_core(),
        }
    }

    fn default_compatible() -> Self {
        Self::default_with_mode(RvnicEnvMode::Compatible)
    }

    fn default_isolated() -> Self {
        Self::default_with_mode(RvnicEnvMode::Isolated)
    }
}

pub struct RvnicEnv {
    pub mode: RvnicEnvMode,
    pub left_ns: Arc<NetNs>,
    pub right_ns: Arc<NetNs>,
    pub left_ip: IpAddr,
    pub right_ip: IpAddr,
    pub rattan_id: String,
    packet_recycler: Option<RvnicPacketRecycler>,
    left_data: Option<Vec<RvnicQueueDataPath>>,
    right_data: Option<Vec<RvnicQueueDataPath>>,
    left_device: Arc<RvnicDevice>,
    right_device: Arc<RvnicDevice>,
    left_recv_core: u32,
    right_recv_core: u32,
}

impl RattanEnv<RvnicDriver> for RvnicEnv {
    type Mode = RvnicEnvMode;
    fn get_rattan_id(&self) -> &str {
        &self.rattan_id
    }
    fn get_rattan_ns(&self) -> Option<Arc<NetNs>> {
        None
    }
    fn get_left_ns(&self) -> Arc<NetNs> {
        self.left_ns.clone()
    }
    fn get_right_ns(&self) -> Arc<NetNs> {
        self.right_ns.clone()
    }
    fn get_mode(&self) -> Self::Mode {
        self.mode
    }
    fn left_ip(&self, i: usize) -> IpAddr {
        assert_eq!(i, 1, "Multiple Rvnic devices are not supported");
        self.left_ip
    }
    fn right_ip(&self, i: usize) -> IpAddr {
        assert_eq!(i, 1, "Multiple Rvnic devices are not supported");
        self.right_ip
    }

    // For legacy reasons, 0 is reserved for `external` connection, which we do not have
    fn left_ip_list(&self) -> Vec<(usize, IpAddr)> {
        vec![(1, self.left_ip)]
    }
    fn right_ip_list(&self) -> Vec<(usize, IpAddr)> {
        vec![(1, self.right_ip)]
    }

    // Multipath is not support for rvnic for now
    fn left_max_id(&self) -> usize {
        1
    }
    fn right_max_id(&self) -> usize {
        1
    }

    fn build_interfaces(
        &mut self,
        runtime: &tokio::runtime::Handle,
    ) -> Result<Vec<InterfaceBuildArtifact<RvnicDriver>>, MetalError> {
        if let Some(packet_recycler) = self.packet_recycler.take() {
            runtime.spawn(packet_recycler.run());
        }

        let (Some(left_data), Some(right_data)) = (self.left_data.take(), self.right_data.take())
        else {
            unreachable!("This function should be called exactly once");
        };

        let to_driver = |data_path: RvnicQueueDataPath,
                         receive_thread_core,
                         self_device: Arc<RvnicDevice>,
                         other_device: Arc<RvnicDevice>| {
            let device_fd = self_device.fd();
            let meta = DriverMetaData {
                self_device,
                other_device,
                device_fd,
                queue_id: data_path.id.queue_id,
                receive_thread_core,
            };
            RvnicDriver::new(data_path.id, data_path.rx, data_path.tx, runtime, meta)
        };

        let left = left_data.into_iter().map(|data| {
            to_driver(
                data,
                self.left_recv_core,
                self.left_device.clone(),
                self.right_device.clone(),
            )
        });
        let right = right_data.into_iter().map(|data| {
            to_driver(
                data,
                self.right_recv_core,
                self.right_device.clone(),
                self.left_device.clone(),
            )
        });

        let result = vec![
            InterfaceBuildArtifact {
                ns_id: 1,
                veth_id: 1,
                name: "left".to_string(),
                drivers: left.collect(),
            },
            InterfaceBuildArtifact {
                ns_id: 2,
                veth_id: 1,
                name: "right".to_string(),
                drivers: right.collect(),
            },
        ];

        Ok(result)
    }
}

struct RvnicQueueDataPath {
    id: QueueID,
    rx: RxRing,
    tx: TxRing,
}

struct RvnicBuildArtifact {
    left_device: RvnicDevice,
    right_device: RvnicDevice,
    left_data: Vec<RvnicQueueDataPath>,
    right_data: Vec<RvnicQueueDataPath>,
    umem: Arc<Umem>,
    packet_recycler: RvnicPacketRecycler,
    packet_recycler_tx: mpsc::UnboundedSender<(QueueID, u64)>,
}

fn build_rvnic_pair(queue_nums: u32) -> rvnic::Result<RvnicBuildArtifact> {
    let mut dev0 = RvnicDevice::open()?;
    let mut dev1 = RvnicDevice::open()?;

    let chunks_per_device = queue_nums * RATTAN_RING_SIZE;

    let umem = UmemBuilder::new()
        .chunk_size(RATTAN_DEFAULT_CHUNK_SIZE)
        .headroom(RATTAN_HEADROOM)
        // `RATTAN_RING_SIZE` chunks per queue, `RVNIC_DEVICES` devices, `queue_nums` queues per device
        .num_chunks(chunks_per_device * RVNIC_DEVICES)
        .build()?;

    let num_chunks = umem.num_chunks();
    let chunk_size = umem.chunk_size();

    tracing::info!(
        "umem size: {} chunks x {}Bytes",
        umem.num_chunks(),
        umem.chunk_size()
    );

    let umem = dev0.register_umem(umem)?;
    dev1.share_umem(&dev0)?;
    tracing::info!("   UMEM registered and shared");

    let dev0_configs = build_queue_configs(queue_nums)?;
    let dev1_configs = build_queue_configs(queue_nums)?;

    let mut dev0_queues = dev0.register_queues(dev0_configs)?;
    let mut dev1_queues = dev1.register_queues(dev1_configs)?;
    tracing::info!("   Registered {} queues on each device", queue_nums);

    let dev0_chunks = chunks_per_device;
    let dev1_chunks = num_chunks - dev0_chunks;

    let dev0_fill = prefill_device_queues(&mut dev0_queues, 0, dev0_chunks, chunk_size);
    let dev1_fill = prefill_device_queues(&mut dev1_queues, dev0_chunks, dev1_chunks, chunk_size);

    tracing::info!(
        "Pre-filled FILL rings: {} chunks to dev0, {} chunks to dev1 ",
        dev0_fill,
        dev1_fill,
    );

    let mut drop_rings = vec![];
    let mut rx_tx_rings = vec![];

    let raw_fds = [dev0.fd(), dev1.fd()];

    for (device_id, queues) in raw_fds.into_iter().zip([dev0_queues, dev1_queues]) {
        for queue_ring in queues {
            let queue_id = QueueID {
                device_fd: device_id,
                queue_id: queue_ring.queue_id,
            };
            drop_rings.push((queue_id, queue_ring.drop));
            rx_tx_rings.push((queue_id, queue_ring.rx, queue_ring.tx));
            // We do not need to use the other 2 rings, and they are dropped here
        }
    }

    let (tx, rx) = mpsc::unbounded_channel::<(QueueID, u64)>();

    let packet_recycler = RvnicPacketRecycler {
        rx,
        drop_rings: HashMap::from_iter(
            drop_rings
                .into_iter()
                .map(|(id, ring)| (id, (ring, Vec::with_capacity(BATCH_SIZE)))),
        ),
    };

    let (left, right): (Vec<_>, Vec<_>) = rx_tx_rings
        .into_iter()
        .partition(|(id, _, _)| id.device_fd == dev0.fd());

    let left_data = left
        .into_iter()
        .map(|(id, rx, tx)| RvnicQueueDataPath { id, rx, tx })
        .collect();

    let right_data = right
        .into_iter()
        .map(|(id, rx, tx)| RvnicQueueDataPath { id, rx, tx })
        .collect();

    Ok(RvnicBuildArtifact {
        left_device: dev0,
        right_device: dev1,
        left_data,
        right_data,
        umem,
        packet_recycler,
        packet_recycler_tx: tx,
    })
}

// Get 10.x.y.1 (right) and 10.x.y.2(left), that 10.x.y.1 is not occupied.
pub fn get_addrs(mode: RvnicEnvMode) -> Result<(IpAddr, (IpAddr, Option<IpAddrLock>)), Error> {
    if !matches!(mode, RvnicEnvMode::Compatible) {
        let left_addr = IpAddr::from(Ipv4Addr::new(10, 99, 0, 1));
        let right_addr = IpAddr::from(Ipv4Addr::new(10, 99, 0, 2));
        return Ok((left_addr, (right_addr, None)));
    }

    let address_in_use: HashSet<IpAddr> = HashSet::from_iter(get_addresses_in_use()?);
    for x in 0..255u16 {
        let x = (x + 99) % 255;
        let x = x as u8;
        for y in 0..255 {
            let left_addr = IpAddr::from(Ipv4Addr::new(10, x, y, 1));
            let right_addr = IpAddr::from(Ipv4Addr::new(10, x, y, 2));
            if address_in_use.contains(&right_addr) {
                continue;
            }
            if let Some(lock) = IpAddrLock::new(right_addr)? {
                return Ok((left_addr, (right_addr, Some(lock))));
            }
        }
    }
    Err(Error::IoError(std::io::Error::other(
        "Failed to get available ip addr for rvnic in host netns",
    )))
}

#[instrument(skip_all, level = "debug", name = "RvnicEnv")]
pub fn get_rvnic_env(config: &RvnicEnvConfig) -> Result<RvnicEnv, Error> {
    // Create network namespaces
    info!(?config);
    let _guard = RVNIC_ENV_LOCK.lock();
    let rand_string: String = rng()
        .sample_iter(&Alphanumeric)
        .take(6)
        .map(char::from)
        .collect();

    let left_netns_name = format!("ns-left-{rand_string}");
    let left_netns = NetNs::new(&left_netns_name)?;
    info!(?left_netns, "Left netns {left_netns_name} created");

    let (right_netns, right_netns_name) = match config.mode {
        RvnicEnvMode::Compatible => (NetNs::current()?, "".to_string()),
        RvnicEnvMode::Isolated => {
            let right_netns_name = format!("ns-right-{rand_string}");
            let right_netns = NetNs::new(&right_netns_name)?;
            info!(
                ?right_netns,
                "Right netns {} created",
                right_netns_name.as_str()
            );
            (right_netns, right_netns_name)
        }
        RvnicEnvMode::Container => {
            return Err(Error::ConfigError(
                "Rvnic does not support container mode".to_string(),
            ))
        }
    };

    // Ignore the `external` veth pairs for now!
    //
    let RvnicBuildArtifact {
        mut left_device,
        mut right_device,
        left_data,
        right_data,
        umem,
        packet_recycler,
        packet_recycler_tx,
    } = build_rvnic_pair(config.num_queues as u32)?;

    if let Err(e) = UMEM.set(umem) {
        match e {
            SetError::AlreadyInitializedError(_) => {
                Err(Error::ConfigError("double init".to_string()))?
            }
            SetError::InitializingError(_) => {
                Err(Error::ConfigError("concurrent init".to_string()))?
            }
        }
    }

    if let Err(e) = RVNIC_PACKET_RECYCLE_TX.set(packet_recycler_tx) {
        match e {
            SetError::AlreadyInitializedError(_) => {
                Err(Error::ConfigError("double init".to_string()))?
            }
            SetError::InitializingError(_) => {
                Err(Error::ConfigError("concurrent init".to_string()))?
            }
        }
    }

    // Start the devices
    left_device
        .start(config.left_napi_core)
        .map_err(MetalError::RvnicError)?;
    right_device
        .start(config.right_napi_core)
        .map_err(MetalError::RvnicError)?;
    tracing::info!("RVNIC devices started");

    // Move the devices into the netns
    move_to_netns(&left_device.device_name()?, &left_netns_name)?;
    move_to_netns(&right_device.device_name()?, &right_netns_name)?;

    let (left_ip, (right_ip, right_ip_lock)) = get_addrs(config.mode)?;

    info!(
        "trying to set up rvnic device interface {} in {} with ip {}",
        left_device.device_name()?,
        left_netns_name,
        left_ip
    );
    configure_interface_in_netns(&left_netns_name, &left_device.device_name()?, left_ip)?;

    if right_netns_name.is_empty() {
        info!(
            "trying to set up rvnic device interface {} with ip {}",
            right_device.device_name()?,
            right_ip
        );
        configure_interface_in_current_netns(&right_device.device_name()?, right_ip)?;
    } else {
        configure_interface_in_netns(&right_netns_name, &right_device.device_name()?, right_ip)?;
    }

    let env = RvnicEnv {
        left_device: Arc::new(left_device),
        right_device: Arc::new(right_device),
        mode: config.mode,
        left_ns: left_netns,
        right_ns: right_netns,
        left_ip,
        right_ip,
        rattan_id: rand_string,
        // Will be consumed when `build_interface` is called for the first time.
        packet_recycler: Some(packet_recycler),
        left_data: Some(left_data),
        right_data: Some(right_data),
        left_recv_core: config.left_recv_core,
        right_recv_core: config.right_recv_core,
    };

    drop(right_ip_lock);
    Ok(env)
}
