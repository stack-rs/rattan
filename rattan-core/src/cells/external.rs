/// External cells are all cells not created by Rattan.
/// Network interfaces, physical or virtual, are examples of external cells.
use std::{
    collections::HashMap,
    fmt::Display,
    sync::{
        atomic::{AtomicU32, AtomicUsize},
        Arc, LazyLock,
    },
};

use async_trait::async_trait;
use bitfield::{BitRange, BitRangeMut};
use parking_lot::RwLock;
use rattan_log::{
    log_entry::general_packet::GeneralPacketType, log_entry::PktAction, FlowDesc, PlainBytes,
    RattanLogOp, RawLogEntry, LOGGING_TX,
};
#[cfg(feature = "serde")]
use serde::Deserialize;
use tokio::{io::unix::AsyncFd, sync::mpsc::UnboundedSender};
use tracing::{debug, error, instrument, warn};

use crate::{
    cells::{Cell, ControlInterface, Egress, Ingress, Packet},
    error::{Error, TokioRuntimeError},
    metal::{
        io::common::{InterfaceDriver, InterfaceReceiver, InterfaceSender},
        veth::VethCell,
    },
    radix::{PacketLogMode, BASE_TS, PKT_LOG_MODE},
};

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct VirtualEthernetId(u16);

impl VirtualEthernetId {
    pub fn new() -> Self {
        Self(0)
    }

    pub fn set_ns_id(&mut self, ns_id: u8) {
        self.0.set_bit_range(15, 8, ns_id);
    }

    pub fn set_ns_id_copied(&mut self, ns_id: u8) -> Self {
        let mut new = *self;
        new.set_ns_id(ns_id);
        new
    }

    pub fn get_ns_id(&self) -> u8 {
        self.0.bit_range(15, 8)
    }

    pub fn set_veth_id(&mut self, id: u8) {
        self.0.set_bit_range(7, 0, id);
    }

    pub fn set_veth_id_copied(&mut self, id: u8) -> Self {
        let mut new = *self;
        new.set_veth_id(id);
        new
    }

    pub fn get_veth_id(&self) -> u8 {
        self.0.bit_range(7, 0)
    }
}

impl Default for VirtualEthernetId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for VirtualEthernetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ns{}-veth{}", self.get_ns_id(), self.get_veth_id())
    }
}

pub struct VirtualEthernetIngress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
{
    sender: tokio::sync::mpsc::Sender<D::Packet>,
    id: VirtualEthernetId,
    log_tx: Option<UnboundedSender<RattanLogOp>>,
    base_ts: i64,
}

impl<D> VirtualEthernetIngress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
{
    pub fn new(
        dev_sender: Vec<Arc<D::Sender>>,
        id: VirtualEthernetId,
        log_tx: Option<UnboundedSender<RattanLogOp>>,
        base_ts: i64,
    ) -> Self {
        let mut senders: Vec<tokio::sync::mpsc::Sender<D::Packet>> = vec![];
        let (dev_tx, dev_rx) = tokio::sync::mpsc::channel(1024);

        for s in dev_sender.iter() {
            let (tx, rx) = tokio::sync::mpsc::channel(1024);
            tokio::spawn(Self::send(rx, s.clone()));
            senders.push(tx);
        }

        tokio::spawn(Self::demux(dev_rx, senders.clone()));
        Self {
            sender: dev_tx,
            id,
            log_tx,
            base_ts,
        }
    }

    async fn demux(
        mut receiver: tokio::sync::mpsc::Receiver<D::Packet>,
        senders: Vec<tokio::sync::mpsc::Sender<D::Packet>>,
    ) {
        let mut index: usize = 0;
        loop {
            if let Some(packet) = receiver.recv().await {
                let _ = senders[index % senders.len()].send(packet).await;
                index += 1;
            }
        }
    }

    async fn send(mut receiver: tokio::sync::mpsc::Receiver<D::Packet>, sender: Arc<D::Sender>) {
        loop {
            if let Some(packet) = receiver.recv().await {
                let _ = sender.send(packet);
            }
        }
    }
}

impl<D> Clone for VirtualEthernetIngress<D>
where
    D: InterfaceDriver + Send + Sync,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
{
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            id: self.id,
            log_tx: self.log_tx.clone(),
            base_ts: self.base_ts,
        }
    }
}

impl<D> Ingress<D::Packet> for VirtualEthernetIngress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
{
    fn enqueue(&self, packet: D::Packet) -> Result<(), Error> {
        if let (Some(&log_mode), Some(log_tx)) = (PKT_LOG_MODE.get(), self.log_tx.as_ref()) {
            log_packet(log_tx, &packet, PktAction::Send, self.base_ts, log_mode);
        }
        Ok(self
            .sender
            .try_send(packet)
            .map_err(|e| TokioRuntimeError::MpscError(e.to_string()))?)
    }
}

struct FlowMap {
    map: RwLock<HashMap<FlowDesc, u32>>,
    id: AtomicU32,
}

impl FlowMap {
    fn new(veth_id: VirtualEthernetId) -> Self {
        let mut id = 0_u32;
        id.set_bit_range(31, 16, veth_id.0);
        Self {
            map: RwLock::new(HashMap::new()),
            id: AtomicU32::new(id),
        }
    }

    fn get_id(&self, desc: FlowDesc, log_tx: &UnboundedSender<RattanLogOp>, base_ts: i64) -> u32 {
        {
            let map = self.map.read();
            if let Some(meta) = map.get(&desc) {
                return *meta;
            }
        }
        let mut map = self.map.write();
        let id = self.id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        map.insert(desc.clone(), id);

        let op = RattanLogOp::Flow(id, base_ts, desc);
        if log_tx.send(op).is_err() {
            cnt_log_op_error();
        }

        id
    }
}

pub struct VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
{
    receiver: tokio::sync::mpsc::Receiver<D::Packet>,
    _id: VirtualEthernetId,
}

impl<D> VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + Send + Sync,
{
    pub fn new(
        driver: Vec<D>,
        id: VirtualEthernetId,
        log_tx: Option<UnboundedSender<RattanLogOp>>,
        base_ts: i64,
    ) -> Result<Self, Error> {
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let flow_map = Arc::new(FlowMap::new(id));

        for d in driver.into_iter() {
            let notify = AsyncFd::new(d.raw_fd())?;
            tokio::spawn(Self::recv(
                flow_map.clone(),
                notify,
                d.into_receiver(),
                tx.clone(),
                log_tx.clone(),
                base_ts,
            ));
        }

        Ok(Self {
            receiver: rx,
            _id: id,
        })
    }

    async fn recv(
        flow_map: Arc<FlowMap>,
        notify: AsyncFd<i32>,
        mut receiver: D::Receiver,
        sender: tokio::sync::mpsc::Sender<D::Packet>,
        log_tx: Option<UnboundedSender<RattanLogOp>>,
        base_ts: i64,
    ) {
        let packet_log_mode = PKT_LOG_MODE.get();
        loop {
            let mut _guard = notify.readable().await.unwrap();

            let Ok(packet) = _guard.try_io(|_fd| receiver.receive()) else {
                continue; // would block
            };

            let mut packet = match packet {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    continue;
                }
                Err(e) => {
                    error!("recv error: {}", e);
                    continue;
                }
            };

            // Getting `Packet::flow_desc()` is not trivial (including inspection of the headers in the payload).
            // Avoid doing so when packet log is not enabled.
            if let (Some(&log_mode), Some(log_tx)) = (packet_log_mode, log_tx.as_ref()) {
                if let Some(desc) = packet.flow_desc() {
                    let id = flow_map.get_id(desc, log_tx, base_ts);
                    packet.set_flow_id(id);
                }
                log_packet(log_tx, &packet, PktAction::Recv, base_ts, log_mode);
            }

            let _ = sender.send(packet).await;
        }
    }
}

#[derive(Default)]
#[repr(transparent)]
struct LogOpErrorCount(AtomicUsize);

impl Drop for LogOpErrorCount {
    fn drop(&mut self) {
        let error_count = self.0.load(std::sync::atomic::Ordering::SeqCst);
        if error_count > 0 {
            warn!("Error count for Sending Log Entry :{}", error_count);
        }
    }
}

static LOG_OP_ERROR_COUNT: LazyLock<LogOpErrorCount> = LazyLock::new(LogOpErrorCount::default);

fn cnt_log_op_error() {
    LOG_OP_ERROR_COUNT
        .0
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn log_packet<T: Packet>(
    tx: &UnboundedSender<RattanLogOp>,
    p: &T,
    action: PktAction,
    base_ts: i64,
    mode: PacketLogMode,
) {
    let ts = ((get_clock_ns() - base_ts) / 1000)
        .max(0)
        .min(u32::MAX as i64) as u32;

    let pkt_len = p.length() as u16;

    match mode {
        PacketLogMode::CompactTCP => {
            // Make it simple as only TCP is supported.
            let mut entry = rattan_log::TCPLogEntry::new();
            entry.general_pkt_entry.pkt_length = pkt_len;
            entry.general_pkt_entry.header.set_pkt_action(action as u8);
            entry.general_pkt_entry.ts = ts;
            entry.tcp_entry.flow_id = p.get_flow_id();

            // Inspect packet buffer
            if let Ok(ether_hdr) = etherparse::Ethernet2HeaderSlice::from_slice(p.as_slice()) {
                #[allow(clippy::single_match)]
                match ether_hdr.ether_type() {
                    etherparse::EtherType::IPV4 => {
                        match etherparse::Ipv4HeaderSlice::from_slice(
                            p.as_slice().get(ether_hdr.slice().len()..).unwrap_or(&[]),
                        ) {
                            Ok(ip_hdr) => {
                                entry.tcp_entry.ip_id = ip_hdr.identification();
                                entry.tcp_entry.ip_frag = unsafe {
                                    // SAFETY:
                                    // Safe as the slice length is checked to be at least
                                    // Ipv4Header::MIN_LEN (20) in the constructor.
                                    u16::from_be_bytes([
                                        *ip_hdr.slice().get_unchecked(6),
                                        *ip_hdr.slice().get_unchecked(7),
                                    ])
                                };
                                entry.tcp_entry.checksum = ip_hdr.header_checksum();
                                #[allow(clippy::single_match)]
                                match ip_hdr.protocol() {
                                    etherparse::IpNumber::TCP => {
                                        if let Ok(tcp_hdr) = etherparse::TcpHeaderSlice::from_slice(
                                            p.as_slice()
                                                .get(
                                                    ether_hdr.slice().len()
                                                        + ip_hdr.slice().len()..,
                                                )
                                                .unwrap_or(&[]),
                                        ) {
                                            entry.tcp_entry.seq = tcp_hdr.sequence_number();
                                            entry.tcp_entry.ack = tcp_hdr.acknowledgment_number();
                                            entry.tcp_entry.flags =
                                                *tcp_hdr.slice().get(13).unwrap();
                                            entry.tcp_entry.dataofs = tcp_hdr.data_offset();
                                            let entry_bytes = entry.as_bytes().to_owned();
                                            if tx.send(RattanLogOp::Entry(entry_bytes)).is_err() {
                                                cnt_log_op_error();
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Err(e) => {
                                tracing::error!("Error parsing IPv4 header: {}", e);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        PacketLogMode::RawIP | PacketLogMode::RawTCP => {
            let flow_id = p.get_flow_id();

            let mut entry = if mode == PacketLogMode::RawIP {
                RawLogEntry::new(GeneralPacketType::RawIP)
            } else {
                RawLogEntry::new(GeneralPacketType::RawTCPIP)
            };

            entry.general_pkt_entry.pkt_length = pkt_len;
            entry.general_pkt_entry.ts = ts;
            entry.general_pkt_entry.header.set_pkt_action(action as u8);

            if let Ok(headers) = etherparse::PacketHeaders::from_ethernet_slice(p.as_slice()) {
                if let (Some(l2), Some(l3), Some(l4)) =
                    (headers.link, headers.net, headers.transport)
                {
                    let l2_len = l2.header_len();
                    let l3_len = l3.header_len();
                    let l4_len = l4.header_len();

                    let (start, end) = if mode == PacketLogMode::RawIP {
                        (l2_len, l2_len + l3_len + l4_len)
                    } else {
                        (l2_len + l3_len, l2_len + l3_len + l4_len)
                    };

                    if let Some(raw) = p.as_slice().get(start..end) {
                        if tx
                            .send(RattanLogOp::RawEntry(flow_id, entry, raw.to_vec()))
                            .is_err()
                        {
                            cnt_log_op_error();
                        }
                    }
                }
            }
        }
    }
    // tracing::debug!(target: "veth::egress::packet_log", "At {} veth {} recv pkt len {} desc {}", ts, id, p.length(), p.desc());
}

fn get_clock_ns() -> i64 {
    nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)
        .map(|ts| ts.tv_sec() * 1_000_000_000 + ts.tv_nsec())
        .unwrap_or(0)
}

#[async_trait]
impl<D> Egress<D::Packet> for VirtualEthernetEgress<D>
where
    D: InterfaceDriver,
    D::Receiver: Send,
    D::Packet: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<D::Packet> {
        return self.receiver.recv().await;
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize))]
#[derive(Debug, Clone)]
pub struct VirtualEthernetConfig {}

pub struct VirtualEthernetControlInterface {
    _config: VirtualEthernetConfig,
}

impl ControlInterface for VirtualEthernetControlInterface {
    type Config = VirtualEthernetConfig;
    fn set_config(&self, _config: VirtualEthernetConfig) -> Result<(), Error> {
        Ok(())
    }
}

pub struct VirtualEthernet<D: InterfaceDriver>
where
    D: InterfaceDriver + Send,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
{
    _cell: Arc<VethCell>,
    ingress: Arc<VirtualEthernetIngress<D>>,
    egress: VirtualEthernetEgress<D>,
    control_interface: Arc<VirtualEthernetControlInterface>,
}

impl<D: InterfaceDriver> VirtualEthernet<D>
where
    D: InterfaceDriver + Send,
    D::Packet: Packet + Send + Sync,
    D::Sender: Send + Sync,
    D::Receiver: Send,
{
    #[instrument(skip_all, name="VirtualEthernet", fields(name = cell.name))]
    pub fn new(cell: Arc<VethCell>, id: VirtualEthernetId) -> Result<Self, Error> {
        debug!("New VirtualEthernet");
        let driver = D::bind_cell(cell.clone())?;
        let dev_senders = driver.iter().map(|d| d.sender()).collect();
        let log_tx = LOGGING_TX.get().cloned();
        let base_ts = *BASE_TS.get_or_init(get_clock_ns);
        Ok(Self {
            _cell: cell,
            ingress: Arc::new(VirtualEthernetIngress::new(
                dev_senders,
                id,
                log_tx.clone(),
                base_ts,
            )),
            egress: VirtualEthernetEgress::new(driver, id, log_tx, base_ts)?,
            control_interface: Arc::new(VirtualEthernetControlInterface {
                _config: VirtualEthernetConfig {},
            }),
        })
    }
}

impl<D> Cell<D::Packet> for VirtualEthernet<D>
where
    D: InterfaceDriver + Send + 'static,
    D::Packet: Packet + Send + Sync + 'static,
    D::Sender: Send + Sync,
    D::Receiver: Send,
{
    type IngressType = VirtualEthernetIngress<D>;
    type EgressType = VirtualEthernetEgress<D>;
    type ControlInterfaceType = VirtualEthernetControlInterface;

    fn sender(&self) -> Arc<Self::IngressType> {
        self.ingress.clone()
    }

    fn receiver(&mut self) -> &mut Self::EgressType {
        &mut self.egress
    }

    fn into_receiver(self) -> Self::EgressType {
        self.egress
    }

    fn control_interface(&self) -> Arc<Self::ControlInterfaceType> {
        self.control_interface.clone()
    }
}
