use crate::{
    error::{Error, VethError},
    metal::{
        netns::NetNs,
        route::{add_arp_entry_with_netns, add_route_with_netns, set_loopback_up_with_netns},
        veth::{MacAddr, VethCell, VethPair, VethPairBuilder},
    },
};
use futures::TryStreamExt;
use once_cell::sync::OnceCell;
use rand::distr::Alphanumeric;
use rand::{rng, Rng};
use rtnetlink::packet_route::{address::AddressAttribute, link::LinkAttribute, route::RouteScope};
use std::{collections::BTreeMap, io::Write, sync::Arc};
use std::{
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
};
use tracing::{debug, error, info, instrument, trace};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

//    ns-left                                                           ns-right
// +-----------+ [Internet]                               [Internet] +-----------+
// |    vL0    |    |                 ns-rattan                 |    |    vR0    |
// |  external | <--+          +---------------------+          +--> |  external |
// |   vL1-L   |               |  vL1-R  [P]   vR1-L |               |   vR1-R   |
// | .1.1.x/32 | <-----------> |.1.1.2/32   .2.1.2/32| <-----------> | .2.1.x/32 |
// |   vL2-L   |               |  vL2-R        vR2-L |               |   vR2-R   |
// | .1.2.y/32 | <-----------> |.1.2.2/32   .2.2.2/32| <-----------> | .2.2.y/32 |
// ~    ...    ~   Veth pairs  ~  ...           ...  ~   Veth pairs  ~    ...    ~
// +-----------+               +---------------------+               +-----------+
//
// Use /32 to avoid route conflict between multiple rattan instances
//
// Left veth pairs use 10.1.a.b, while right veth pairs use 10.2.a.b ,
// where `a` is veth pair id (1..=254), and `b` is chosen according to the following rules:
// 1. In `ns-rattan`, `b` = 2;
// 2. In `ns-left` and `ns-right`, the same `a` corresponds to the same `b`;
// 3. If `ns-right` is NOT Compatible, `b` = 1;
// 4. If `ns-right` is Compatible, `b` is chosen without conflicting with existing IP.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum VethPairGroup {
    Left = 1,
    Right = 2,
}

const VETH_COUNT_MAX: usize = 254;

fn get_veth_ip_address(pair_group: VethPairGroup, pair_id: usize, suffix: u8) -> (IpAddr, u8) {
    assert!((1..=VETH_COUNT_MAX).contains(&pair_id));
    (
        IpAddr::V4(Ipv4Addr::new(10, pair_group as u8, pair_id as u8, suffix)),
        32,
    )
}

fn get_veth_mac_address(pair_group: VethPairGroup, pair_id: usize, suffix: u8) -> MacAddr {
    assert!((1..=VETH_COUNT_MAX).contains(&pair_id));
    // just ensure that I/G bit is 0
    [0x38, 0x7e, 0x58, pair_group as u8, pair_id as u8, suffix].into()
}

fn get_addresses_in_use() -> Result<Vec<IpAddr>, Error> {
    debug!("Get addresses in use");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            error!("Failed to build rtnetlink runtime");
            Error::TokioRuntimeError(e.into())
        })?;
    let _guard = rt.enter();
    let (conn, rtnl_handle, _) = rtnetlink::new_connection()?;
    rt.spawn(conn);

    let mut addresses = vec![];
    rt.block_on(async {
        let mut links = rtnl_handle.address().get().execute();
        while let Ok(Some(address_msg)) = links.try_next().await {
            for address_attr in address_msg.attributes {
                if let AddressAttribute::Address(address) = address_attr {
                    trace!(?address, ?address_msg.header.prefix_len, "Get address");
                    addresses.push(address);
                }
            }
        }
    });
    debug!(?addresses, "Addresses in use");
    Ok(addresses)
}

const IP_LOCK_DIR: &str = "/tmp/rattan/ip_lock";

struct IpAddrLock {
    file_dir: String,
}

impl IpAddrLock {
    /// Lock an IP address by creating a file with the same name as the it
    fn new(ip: IpAddr) -> Result<Option<Self>, std::io::Error> {
        // for ipv6, ':' may be illegal as a filename
        let ip_str = format!("{ip}").replace(':', "_");
        let file_dir = format!("{IP_LOCK_DIR}/{ip_str}");
        match std::fs::File::create_new(&file_dir) {
            // Lock successfully
            Ok(_) => Ok(Some(IpAddrLock { file_dir })),
            // Lock file already exists
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            // Other error
            Err(e) => Err(e),
        }
    }
}

impl Drop for IpAddrLock {
    /// Unlock by removing the lock file
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file_dir);
    }
}

lazy_static::lazy_static! {
    static ref STD_ENV_LOCK: Arc<parking_lot::Mutex<()>> = Arc::new(parking_lot::Mutex::new(()));
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Default, Copy, Eq, PartialEq)]
pub enum StdNetEnvMode {
    #[default]
    Compatible,
    Isolated,
    Container,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Default)]
pub enum IODriver {
    #[default]
    Packet,
    Xdp,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Default)]
pub enum NetDevice {
    #[default]
    Veth,
}

fn default_veth_count() -> usize {
    1
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone)]
pub struct StdNetEnvConfig {
    #[cfg_attr(feature = "serde", serde(default))]
    pub mode: StdNetEnvMode,

    #[cfg_attr(feature = "serde", serde(default = "default_veth_count"))]
    pub left_veth_count: usize,
    #[cfg_attr(feature = "serde", serde(default = "default_veth_count"))]
    pub right_veth_count: usize,

    #[cfg_attr(feature = "serde", serde(default))]
    pub left_external: Option<NetDevice>,

    #[cfg_attr(feature = "serde", serde(default))]
    pub right_external: Option<NetDevice>,

    // TODO(minhuw): pretty sure these two configs should not be here
    // but let it be for now
    #[cfg_attr(feature = "serde", serde(default))]
    pub client_cores: Vec<usize>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub server_cores: Vec<usize>,
}

impl Default for StdNetEnvConfig {
    fn default() -> Self {
        Self {
            mode: StdNetEnvMode::default(),
            left_veth_count: default_veth_count(),
            right_veth_count: default_veth_count(),
            client_cores: vec![],
            server_cores: vec![],
            left_external: None,
            right_external: None,
        }
    }
}

pub struct StdNetEnv {
    pub left_ns: Arc<NetNs>,
    pub rattan_ns: Arc<NetNs>,
    pub right_ns: Arc<NetNs>,
    // For legacy reasons, index 0 is reserved for an external veth pair.
    // This external veth pair is not fully implemented yet and is constructed only optionally.
    //
    // If an additional external NIC is required (e.g., a TUN device or extra veth pairs),
    // it should be represented as a separate field in `StdNetEnv`, rather than being included
    // in the `left_pairs` or `right_pairs` BTreeMaps.
    //
    // TODO: Move the external veth pair out of the BTreeMaps.
    pub left_pairs: BTreeMap<usize, Arc<VethPair>>,
    pub right_pairs: BTreeMap<usize, Arc<VethPair>>,
}

impl StdNetEnv {
    pub fn left_default_pair(&self) -> &Arc<VethPair> {
        self.left_pairs.get(&0).unwrap()
    }
    pub fn right_default_pair(&self) -> &Arc<VethPair> {
        self.right_pairs.get(&0).unwrap()
    }
    pub fn left_max_id(&self) -> usize {
        *self.left_pairs.last_key_value().unwrap().0
    }
    pub fn right_max_id(&self) -> usize {
        *self.right_pairs.last_key_value().unwrap().0
    }
}

/// Try to lock an IP suffix x (e.g. 10.pair_group.pair_id.x) in current netns
fn lock_address_suffix(
    pair_group: VethPairGroup,
    pair_id: usize,
) -> Result<(IpAddrLock, u8), Error> {
    // start from x=1
    let mut addr_suffix = 1;
    let lock = loop {
        // try to create a lock file
        let ip = get_veth_ip_address(pair_group, pair_id, addr_suffix).0;
        let lock = IpAddrLock::new(ip)?;

        // if locked and not in use, return
        let addresses_in_use = get_addresses_in_use()?;
        if let Some(l) = lock {
            if !addresses_in_use.contains(&ip) {
                break l;
            }
        }

        debug!("Address suffix {} in use, try next.", addr_suffix);
        // get next x < 255, which != 2 and not in use
        loop {
            addr_suffix += 1;
            if addr_suffix == 2 {
                addr_suffix += 1;
            }
            if addr_suffix == 255 {
                // all in use, fail
                return Err(VethError::CreateVethPairError(
                    "No available address suffix for right veth".to_string(),
                )
                .into());
            }
            if !addresses_in_use.contains(&get_veth_ip_address(pair_group, pair_id, addr_suffix).0)
            {
                break;
            }
        }
    };
    info!("Successfully lock address suffix {}", addr_suffix);
    Ok((lock, addr_suffix))
}

#[instrument(skip_all, level = "debug")]
pub fn get_std_env(config: &StdNetEnvConfig) -> Result<StdNetEnv, Error> {
    // Check veth counts
    if config.left_veth_count < 1
        || config.right_veth_count < 1
        || config.left_veth_count > VETH_COUNT_MAX
        || config.right_veth_count > VETH_COUNT_MAX
    {
        return Err(Error::ConfigError("Invalid veth count".to_string()));
    }

    // Create network namespaces
    trace!(?config);
    let _guard = STD_ENV_LOCK.lock();
    let rand_string: String = rng()
        .sample_iter(&Alphanumeric)
        .take(6)
        .map(char::from)
        .collect();
    let mut kmesg_logger = std::fs::OpenOptions::new().write(true).open("/dev/kmsg")?;
    let instance_id = crate::radix::INSTANCE_ID.get_or_init(|| {
        // get env var from RATTAN_INSTANCE_ID
        std::env::var("RATTAN_INSTANCE_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
    });
    let mut buf = Vec::new();
    writeln!(
        buf,
        "rattan instance id {instance_id} create ns with rand_string {rand_string}"
    )?;
    kmesg_logger.write_all(&buf)?;
    kmesg_logger.flush()?;

    let left_netns_name = format!("ns-left-{rand_string}");
    let left_netns = NetNs::new(&left_netns_name)?;
    trace!(?left_netns, "Left netns {left_netns_name} created");

    let rattan_netns_name = format!("ns-rattan-{rand_string}");
    let rattan_netns = NetNs::new(&rattan_netns_name)?;
    trace!(?rattan_netns, "Rattan netns {rattan_netns_name} created");

    let right_netns_name = format!("ns-right-{rand_string}");
    let right_netns = match config.mode {
        StdNetEnvMode::Compatible => NetNs::current()?,
        StdNetEnvMode::Isolated => NetNs::new(&right_netns_name)?,
        StdNetEnvMode::Container => NetNs::new(&right_netns_name)?,
    };
    trace!(?right_netns, "Right netns {right_netns_name} created");

    // Build veth0 for left and right, which are reserved for external connection, but ignore it for now

    let left_veth0 = match config.left_external {
        Some(NetDevice::Veth) => VethPairBuilder::new()
            .name(
                format!("vL0-L-{rand_string}"),
                format!("vL0-R-{rand_string}"),
            )
            .namespace(Some(left_netns.clone()), Some(rattan_netns.clone()))
            .mac_addr(
                [0x38, 0x7e, 0x58, 0xe7, 1, 1].into(),
                [0x38, 0x7e, 0x58, 0xe7, 1, 2].into(),
            )
            .ip_addr(
                (IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 32),
                (IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2)), 32),
            )
            .build(0)?
            .into(),
        None => None,
    };

    let right_veth0 = match config.right_external {
        Some(NetDevice::Veth) => VethPairBuilder::new()
            .name(
                format!("vR0-L-{rand_string}"),
                format!("vR0-R-{rand_string}"),
            )
            .namespace(Some(rattan_netns.clone()), Some(right_netns.clone()))
            .mac_addr(
                [0x38, 0x7e, 0x58, 0xe7, 2, 2].into(),
                [0x38, 0x7e, 0x58, 0xe7, 2, 1].into(),
            )
            .ip_addr(
                (IpAddr::V4(Ipv4Addr::new(192, 168, 2, 2)), 32),
                (IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1)), 32),
            )
            .build(0)?
            .into(),
        None => None,
    };

    // lock IPs for right veth pairs
    std::fs::create_dir_all(IP_LOCK_DIR)?;
    let mut addr_locks = vec![];
    let mut suffixes = vec![0u8];
    match config.mode {
        StdNetEnvMode::Compatible => {
            for pair_id in 1..=config.right_veth_count {
                let (lock, suffix) = lock_address_suffix(VethPairGroup::Right, pair_id)?;
                suffixes.push(suffix);
                addr_locks.push(lock);
            }
        }
        _ => {
            suffixes.resize(config.right_veth_count + 1, 1);
        }
    }

    // Build right veth pairs
    let mut right_veth_pairs = vec![];
    for (pair_id, suffix) in suffixes.iter().enumerate().skip(1) {
        right_veth_pairs.push(
            VethPairBuilder::new()
                .name(
                    format!("vR{pair_id}-L-{rand_string}"),
                    format!("vR{pair_id}-R-{rand_string}"),
                )
                .namespace(Some(rattan_netns.clone()), Some(right_netns.clone()))
                .mac_addr(
                    get_veth_mac_address(VethPairGroup::Right, pair_id, 2),
                    get_veth_mac_address(VethPairGroup::Right, pair_id, *suffix),
                )
                .ip_addr(
                    get_veth_ip_address(VethPairGroup::Right, pair_id, 2),
                    get_veth_ip_address(VethPairGroup::Right, pair_id, *suffix),
                )
                .build(pair_id)?,
        );
    }

    // Build left veth pairs
    suffixes.resize(config.left_veth_count + 1, 1);

    let mut left_veth_pairs = vec![];
    for (pair_id, suffix) in suffixes.iter().enumerate().skip(1) {
        left_veth_pairs.push(
            VethPairBuilder::new()
                .name(
                    format!("vL{pair_id}-L-{rand_string}"),
                    format!("vL{pair_id}-R-{rand_string}"),
                )
                .namespace(Some(left_netns.clone()), Some(rattan_netns.clone()))
                .mac_addr(
                    get_veth_mac_address(VethPairGroup::Left, pair_id, *suffix),
                    get_veth_mac_address(VethPairGroup::Left, pair_id, 2),
                )
                .ip_addr(
                    get_veth_ip_address(VethPairGroup::Left, pair_id, *suffix),
                    get_veth_ip_address(VethPairGroup::Left, pair_id, 2),
                )
                .build(pair_id)?,
        );
    }

    // TODO: currently we comment this due to privilege issue
    // {
    //     // TODO(haixuan): could you please replace this with Netlink version when
    //     // time is appropriate?
    //     let _ns_guard = NetNsGuard::new(veth_pair_left.left.namespace.clone())?;
    //     std::process::Command::new("tc")
    //         .args([
    //             "qdisc",
    //             "add",
    //             "dev",
    //             &veth_pair_left.left.name,
    //             "root",
    //             "handle",
    //             "1:",
    //             "fq",
    //         ])
    //         .spawn()
    //         .unwrap()
    //         .wait()
    //         .unwrap();

    //     // we need to send packets to cores belonging to left and rights.
    //     // otherwise networking processing of left and right is done on
    //     // rattan's cores
    //     set_rps_cores(veth_pair_left.left.name.as_str(), &config.left_cores);
    // }

    // TODO: currently we comment this due to privilege issue
    // {
    //     // TODO(haixuan): could you please replace this with Netlink version when
    //     // time is appropriate?
    //     let _ns_guard: NetNsGuard = NetNsGuard::new(veth_pair_right.right.namespace.clone())?;
    //     std::process::Command::new("tc")
    //         .args([
    //             "qdisc",
    //             "add",
    //             "dev",
    //             &veth_pair_right.right.name,
    //             "root",
    //             "handle",
    //             "1:",
    //             "fq",
    //         ])
    //         .spawn()
    //         .unwrap()
    //         .wait()
    //         .unwrap();

    //     set_rps_cores(veth_pair_right.right.name.as_str(), &config.right_cores);
    // }

    // Set veth1 as the default route of left and right namespaces
    info!("Set default route");

    debug!("Set default route for left namespace");

    debug!("Set left interface[0] as default interface");
    add_route_with_netns(
        right_veth_pairs[0].right.ip_addr,
        None,
        left_veth_pairs[0].left.index,
        left_netns.clone(),
        RouteScope::Link,
    )?;

    debug!("Set left interface[0]'s ip as default route");
    add_route_with_netns(
        None,
        Some(right_veth_pairs[0].right.ip_addr.0),
        left_veth_pairs[0].left.index,
        left_netns.clone(),
        RouteScope::Universe,
    )?;

    debug!("Set default route for right namespace");
    match config.mode {
        StdNetEnvMode::Compatible => {
            for left_veth in left_veth_pairs.iter() {
                add_route_with_netns(
                    left_veth.left.ip_addr,
                    None,
                    right_veth_pairs[0].right.index,
                    right_netns.clone(),
                    RouteScope::Link,
                )?;
            }
        }
        _ => {
            add_route_with_netns(
                left_veth_pairs[0].left.ip_addr,
                None,
                right_veth_pairs[0].right.index,
                right_netns.clone(),
                RouteScope::Link,
            )?;
            add_route_with_netns(
                None,
                Some(left_veth_pairs[0].left.ip_addr.0),
                right_veth_pairs[0].right.index,
                right_netns.clone(),
                RouteScope::Universe,
            )?;
        }
    }

    // Set the default neighbors of left and right namespaces
    info!("Set default neighbors");
    for left_veth in left_veth_pairs.iter() {
        for right_veth in right_veth_pairs.iter() {
            add_arp_entry_with_netns(
                right_veth.right.ip_addr.0,
                right_veth.right.mac_addr,
                left_veth.left.index,
                left_netns.clone(),
            )?;
            add_arp_entry_with_netns(
                left_veth.left.ip_addr.0,
                left_veth.left.mac_addr,
                right_veth.right.index,
                right_netns.clone(),
            )?;
        }
    }

    info!("Set lo interface up");
    set_loopback_up_with_netns(left_netns.clone())?;
    set_loopback_up_with_netns(rattan_netns.clone())?;
    set_loopback_up_with_netns(right_netns.clone())?;

    fn build_pairs(
        veth_0: Option<Arc<VethPair>>,
        others: Vec<Arc<VethPair>>,
    ) -> BTreeMap<usize, Arc<VethPair>> {
        let mut result = BTreeMap::from_iter(
            others
                .into_iter()
                .enumerate()
                .map(|(id, veth)| (id + 1, veth)),
        );
        if let Some(veth_0) = veth_0 {
            result.insert(0, veth_0);
        }
        result
    }

    Ok(StdNetEnv {
        left_ns: left_netns,
        rattan_ns: rattan_netns,
        right_ns: right_netns,
        left_pairs: build_pairs(left_veth0, left_veth_pairs),
        right_pairs: build_pairs(right_veth0, right_veth_pairs),
    })
}

#[derive(Debug)]
pub struct ContainerEnv {
    pub veth_list: Vec<VethCell>,
    pub fake_peer: Arc<VethCell>,
}

#[instrument(skip_all, level = "debug")]
pub fn get_container_env() -> crate::error::Result<ContainerEnv> {
    debug!("Getting all veth cells");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            error!("Failed to build rtnetlink runtime");
            Error::TokioRuntimeError(e.into())
        })?;
    let _guard = rt.enter();
    let (conn, rtnl_handle, _) = rtnetlink::new_connection()?;
    rt.spawn(conn);

    // Get all link cell
    let mut links = rtnl_handle.link().get().execute();
    let mut veth_list = vec![];
    rt.block_on(async {
        while let Some(msg) = links.try_next().await.unwrap() {
            let mut name: Option<String> = None;
            let index = msg.header.index;
            let mut mac_addr: Option<MacAddr> = None;
            let mut ip_addr: Option<(IpAddr, u8)> = None;
            let namespace: Arc<NetNs> = NetNs::current().unwrap();

            // Get link attributes
            for link_attr in msg.attributes {
                match link_attr {
                    LinkAttribute::IfName(n) => {
                        name = Some(n);
                    }
                    LinkAttribute::Address(m) => {
                        mac_addr = Some(MacAddr::new(m.try_into().unwrap()));
                    }
                    _ => {}
                }
            }

            // Skip lo cells
            if let (Some(name), Some(mac_addr)) = (name, mac_addr) {
                if name.starts_with("lo") {
                    continue;
                }
                // Get ip address attributes
                if let Ok(Some(address_msg)) = rtnl_handle
                    .address()
                    .get()
                    .set_link_index_filter(index)
                    .execute()
                    .try_next()
                    .await
                {
                    for address_attr in address_msg.attributes {
                        if let AddressAttribute::Address(address) = address_attr {
                            ip_addr = Some((address, address_msg.header.prefix_len));
                        }
                    }
                    if ip_addr.is_none() {
                        continue;
                    }

                    let veth = VethCell {
                        name,
                        index,
                        mac_addr,
                        ip_addr: ip_addr.unwrap(),
                        peer: OnceCell::new(),
                        namespace,
                    };

                    debug!(?veth, "Get veth cell");
                    veth_list.push(veth);
                }
            }
        }
    });

    // FIXME: add arp for each veth
    veth_list.sort_by(|a, b| a.index.cmp(&b.index));

    // FIXME: fake a peer veth cell to get mac address
    let mut fake_peer = veth_list[0].clone();
    fake_peer.mac_addr = MacAddr::from_str("ff:ff:ff:ff:ff:ff").unwrap();
    let fake_peer = Arc::new(fake_peer);
    for veth in &veth_list {
        veth.peer.set(Arc::downgrade(&fake_peer)).unwrap();
    }
    Ok(ContainerEnv {
        veth_list,
        fake_peer,
    })
}
