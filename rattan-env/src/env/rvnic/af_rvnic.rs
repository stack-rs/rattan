use std::{os::fd::AsRawFd, sync::Arc};

use etherparse::{Ethernet2Header, Ipv4Header};

use rvnic::{sys::RattanDesc, RxRing, TxRing};
use tokio::{
    runtime::Handle,
    sync::mpsc,
    time::{Duration, Instant},
};
use tracing::instrument;

use super::constants::*;
use super::rvnic_env::{QueueID, RVNIC_PACKET_RECYCLE_TX, UMEM};
use super::DriverMetaData;
use crate::{
    common::*,
    env::rvnic::utils::{set_cpu_affinity, Epoll},
    timer::Timer,
    Packet,
};

#[derive(Debug)]
pub struct RvnicPacket {
    desc: RattanDesc,
    timestamp: Instant,
    // This tells when we drop this packet in Rattan, to which FILL Ring
    // should we return the buffer!
    received_from: QueueID,
    // If this is false, we need to recycle the token
    sent: bool,

    header: Option<Vec<u8>>,
}

impl Drop for RvnicPacket {
    fn drop(&mut self) {
        if !self.sent {
            if let Some(tx) = RVNIC_PACKET_RECYCLE_TX.get() {
                tx.send((self.received_from, self.desc.token)).ok();
                tracing::trace!(
                    target = "RVNIC",
                    "dropped packet on {:?}, recycling token {}",
                    self.received_from,
                    self.desc.token
                )
            }
        }
    }
}

fn try_get_slice_from_umem(desc: &RattanDesc) -> Option<&[u8]> {
    let umem = UMEM.get()?;
    let chunk_index = umem.offset_to_index(desc.addr)?;
    let data_ptr = umem.data_ptr(chunk_index)?;
    let data_len = desc.len as usize;
    unsafe { Some(std::slice::from_raw_parts(data_ptr, data_len)) }
}

fn try_copy_to_umem(desc: &RattanDesc, header: &[u8]) -> Option<()> {
    let umem = UMEM.get()?;
    let chunk_index = umem.offset_to_index(desc.addr)?;
    let dest_ptr = umem.data_ptr(chunk_index)?;
    let copy_len = std::cmp::min(desc.len as usize, header.len());
    unsafe {
        std::ptr::copy_nonoverlapping(header.as_ptr(), dest_ptr, copy_len);
    }
    Some(())
}

impl Packet for RvnicPacket {
    type PacketGenerator = ();
    fn empty(_maximum: usize, _generator: &Self::PacketGenerator) -> Self {
        unimplemented!("Rvnic packet can not be built directly")
    }
    fn from_raw_buffer(_buf: &[u8]) -> Self {
        unimplemented!("Rvnic packet can not be built directly")
    }

    fn as_raw_buffer(&mut self) -> &mut [u8] {
        // Copy data from UMEM to header buffer. It is the rvnic driver's responsibility
        // to copy back any modification to the skb in kernel from the UMEM, and we need
        // to copy back from `self.header` to the UMEM when the packet leaves Rattan
        // and was sent on the TX path, which is done in the function `try_copy_to_umem`.
        let header = self
            .header
            .get_or_insert_with(|| try_get_slice_from_umem(&self.desc).unwrap_or(&[]).to_vec());
        header.as_mut_slice()
    }

    fn length(&self) -> usize {
        self.desc.get_skb_len() as usize
    }
    fn l2_length(&self) -> usize {
        self.desc.get_skb_len() as usize
    }
    fn l3_length(&self) -> usize {
        self.desc.get_skb_len().saturating_sub(14) as usize
    }
    fn as_slice(&self) -> &[u8] {
        try_get_slice_from_umem(&self.desc).unwrap_or(&[])
    }

    fn ether_hdr(&self) -> Option<Ethernet2Header> {
        None
    }
    fn ip_hdr(&self) -> Option<Ipv4Header> {
        None
    }
    fn get_timestamp(&self) -> Instant {
        self.timestamp
    }
    fn set_timestamp(&mut self, timestamp: Instant) {
        self.delay_until(timestamp);
    }
    fn delay_by(&mut self, delay: Duration) {
        self.timestamp += delay;
    }
    fn delay_until(&mut self, timestamp: Instant) {
        self.timestamp = timestamp;
    }
}

pub struct RvnicSender {
    sender: mpsc::Sender<RvnicPacket>,
}

impl RvnicSender {
    pub fn new(sender: mpsc::Sender<RvnicPacket>) -> RvnicSender {
        Self { sender }
    }
}

impl InterfaceSender<RvnicPacket> for RvnicSender {
    fn send(&self, packet: RvnicPacket) -> std::io::Result<()> {
        // TODO(minhuw): handle errors more carefully here
        let _ = self.sender.try_send(packet);
        Ok(())
    }

    fn send_bulk<Iter, T>(&self, packets: Iter) -> std::io::Result<usize>
    where
        T: Into<RvnicPacket>,
        Iter: IntoIterator<Item = T>,
        Iter::IntoIter: ExactSizeIterator,
    {
        let packets = packets.into_iter().map(|packet| packet.into());

        let len = packets.len();

        //TODO(minhuw): currently we return error even if part of packets are sent
        // maybe we should distinguish between partial success and total failure
        for packet in packets {
            self.sender
                .blocking_send(packet)
                .map_err(|_| std::io::Error::other("send error"))?;
        }

        Ok(len)
    }
}

pub struct RvnicReceiver {
    id: QueueID,
    rx_ring: RxRing,
    meta_data: DriverMetaData,
}

impl RvnicReceiver {
    fn new(id: QueueID, rx_ring: RxRing, meta_data: DriverMetaData) -> Self {
        Self {
            id,
            rx_ring,
            meta_data,
        }
    }
}

impl InterfaceReceiver<RvnicPacket> for RvnicReceiver {
    // Rvnic has native support for batch receiving, so we
    // are using `receive_bulk` to implement `receive`.
    fn receive(&mut self) -> std::io::Result<Option<RvnicPacket>> {
        unimplemented!("Separate thread is used for receiving from rvnic")
    }

    fn receive_bulk(&mut self) -> std::io::Result<Vec<RvnicPacket>> {
        unimplemented!("Separate thread is used for receiving from rvnic")
    }
}

pub struct RvnicDriver {
    receiver: RvnicReceiver,
    sender: Arc<RvnicSender>,
    queue_id: QueueID,
    #[allow(unused)] // To be done: error handling
    forwarder: tokio::task::JoinHandle<()>,
    // forwarder: std::thread::JoinHandle<()>,
}

impl RvnicDriver {
    // Busy polling to recv packets, not used for now
    // TODO(lethe) : Determine which is better, this or using a
    #[allow(unused)]
    fn sending_thread(
        mut receiver: mpsc::Receiver<RvnicPacket>,
        mut tx_ring: TxRing,
        meta: DriverMetaData,
    ) {
        let mut packets = Vec::with_capacity(BATCH_SIZE);
        'end: loop {
            let target = (tx_ring.available() as usize).min(BATCH_SIZE);
            let start = Instant::now();
            while packets.len() < target
                && start.elapsed() < Duration::from_micros(SEND_BATCH_MAX_TIME_US)
            {
                match receiver.try_recv() {
                    Ok(packet) => packets.push(packet),
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        break 'end;
                    }
                    _ => {
                        std::thread::yield_now();
                        continue;
                    }
                }
            }
            if packets.is_empty() {
                continue;
            }
            let mut send_packets = Vec::with_capacity(BATCH_SIZE);
            std::mem::swap(&mut packets, &mut send_packets);
            let _ = Self::send(&mut tx_ring, send_packets, &meta);
        }
        let _ = Self::send(&mut tx_ring, packets, &meta);

        tracing::warn!("Rvnic send thread for {} exited!", meta.device_fd);
    }
}

/// Two things are done here:
///     1. Mark the `sent` as true, which would be checked during the `drop()` of the `RvnicPacket`,
///        so that the packet would be ignored in the recycling token procedure.
///     2. Copy back the `self.header` back to the umem.
fn packet_send_prepare(packet: impl Into<RvnicPacket>) -> (RattanDesc, (QueueID, u64)) {
    let mut packet = packet.into();
    // Avoid recycling tokens!
    packet.sent = true;
    let token = packet.desc.token;
    let desc = packet.desc;

    // Make the constant compare first, make it easier for the compiler's optimizier.
    // Warning: As no cells we currently have modifies the header, this function is never tested!
    if RATTAN_HEADER_SIZE != 0 {
        if let Some(header) = packet.header.take() {
            // XXX: Ignore error handling here
            try_copy_to_umem(&desc, header.as_slice());
        }
    }

    (desc, (packet.received_from, token))
}

impl RvnicDriver {
    #[instrument(name = "Rvnic send", skip_all)]
    fn send<Iter, T>(
        tx: &mut TxRing,
        packets: Iter,
        meta: &DriverMetaData,
    ) -> std::io::Result<usize>
    where
        T: Into<RvnicPacket>,
        Iter: IntoIterator<Item = T>,
        Iter::IntoIter: ExactSizeIterator,
    {
        let (packets, loss_report): (Vec<_>, Vec<_>) =
            packets.into_iter().map(packet_send_prepare).unzip();

        let sent = tx.produce(&packets);
        tracing::debug!(target: "rvnic", "[{}]Send batch {}/{}", meta.device_fd, packets.len(), sent);
        meta.kick_self();
        if sent < packets.len() {
            for to_report in loss_report.into_iter().skip(sent) {
                if let Some(tx) = RVNIC_PACKET_RECYCLE_TX.get() {
                    tx.send(to_report).ok();
                }
            }
            tracing::warn!(
                target = "RVNIC",
                "{} packets dropped on tx ring unexpectedly",
                packets.len() - sent
            );
        }
        Ok(sent)
    }

    // Have not decided how to send!
    async fn buffered_send(
        mut receiver: mpsc::Receiver<RvnicPacket>,
        mut tx_ring: TxRing,
        meta: DriverMetaData,
    ) {
        let mut timer = Timer::new().expect("Failed to create timer for batch sending");
        let mut packets = Vec::with_capacity(BATCH_SIZE);

        // TODO(minhuw): it should not leave forever. But let is be now.
        // we should stop the buffered send task when the cell exists.
        loop {
            let target = (tx_ring.available() as usize).min(BATCH_SIZE);
            let ddl = Instant::now() + Duration::from_micros(SEND_BATCH_MAX_TIME_US);
            while packets.len() < target {
                let to_wait = ddl.duration_since(Instant::now());
                if to_wait < Duration::from_micros(SEND_BATCH_MIN_SLEEP_US) {
                    break;
                }
                tokio::select! {
                    packet = receiver.recv() => {
                        if let Some(packet) = packet {
                            packets.push(packet);
                            // break;
                        }
                    },
                    _ = timer.sleep(to_wait) => {break;}
                }
            }
            if packets.is_empty() {
                tokio::task::yield_now().await;
                continue;
            }
            let mut send_packets = Vec::with_capacity(BATCH_SIZE);
            std::mem::swap(&mut packets, &mut send_packets);
            let _ = Self::send(&mut tx_ring, send_packets, &meta);
        }
    }
}

impl RvnicDriver {
    pub(super) fn new(
        queue_id: QueueID,
        rx_ring: RxRing,
        tx_ring: TxRing,
        runtime: &Handle,
        meta: DriverMetaData,
    ) -> Self {
        let (tx, rx) = mpsc::channel(SEND_BUFFER_SIZE);
        let sender = Arc::new(RvnicSender { sender: tx });
        let receiver = RvnicReceiver::new(queue_id, rx_ring, meta.clone());
        tracing::info!(target: "RVNIC", "Built driver for {:?}", queue_id);

        Self {
            receiver,
            sender,
            queue_id,
            forwarder: runtime.spawn(RvnicDriver::buffered_send(rx, tx_ring, meta)),
            // forwarder: std::thread::spawn(|| RvnicDriver::sending_thread(rx, tx_ring, meta)),
        }
    }
}

impl InterfaceDriver for RvnicDriver {
    type Packet = RvnicPacket;
    type Sender = RvnicSender;
    type Receiver = RvnicReceiver;

    fn raw_fd(&self) -> i32 {
        self.queue_id.device_fd
    }

    fn sender(&self) -> Arc<Self::Sender> {
        self.sender.clone()
    }

    fn receiver(&mut self) -> &mut Self::Receiver {
        &mut self.receiver
    }

    fn into_receiver(self) -> Self::Receiver {
        self.receiver
    }

    fn use_blocking_recv() -> bool {
        true
    }
    #[instrument(name = "Rvnic recv", skip_all)]
    fn recv_thread(mut receiver: Self::Receiver, tx: tokio::sync::mpsc::Sender<Self::Packet>) {
        // Set core affinity
        let cpu = receiver.meta_data.receive_thread_core;
        tracing::info!("Trying to set CPU affinity to {}", cpu);
        if set_cpu_affinity(cpu).is_err() {
            tracing::warn!("Failed to set CPU affinity");
        }

        let self_raw_fd = receiver.meta_data.self_device.as_raw_fd();

        let epoll_fd = Epoll::new()
            .inspect_err(|e| {
                tracing::error!(
                    ?e,
                    "Failed to get Rvnic receiving epoll fd for fd {}",
                    self_raw_fd
                )
            })
            .expect("Failed to get Rvnic receiving epoll fd");

        epoll_fd
            .add(self_raw_fd, self_raw_fd as u64)
            .inspect_err(|e| tracing::error!(?e, "Failed to add epoll fd"))
            .expect("Failed to add epoll fd");

        let mut events = vec![libc::epoll_event { events: 0, u64: 0 }; 32];

        for loop_cnt in 0..u64::MAX {
            let Ok(nfds) = epoll_fd
                .wait(&mut events, RECV_EPOLL_MAX_WAIT_MS)
                .inspect_err(|e| tracing::error!(?e, "Failed to wait on epoll"))
            else {
                break;
            };

            // Make it simple to avoid subtle performance drawback.
            #[allow(clippy::needless_range_loop)]
            for i in 0..nfds {
                let ev = events[i];
                // Always check event flags!
                if ev.events & (libc::EPOLLERR as u32) != 0 {
                    let error_fd = ev.u64;
                    tracing::error!(?error_fd, "Failed on raw fd ");
                    continue;
                }
                // We only added one fd. So just drain all the events on the rx ring!

                let mut batch_count = 0;
                let mut to_receive = [RattanDesc::default(); BATCH_SIZE];
                loop {
                    let filled = receiver.rx_ring.consume(&mut to_receive);
                    if filled == 0 {
                        break;
                    }
                    batch_count += filled;

                    let timestamp = Instant::now();
                    let result = to_receive.into_iter().take(filled).map(|desc| RvnicPacket {
                        desc,
                        header: None,
                        timestamp,
                        received_from: receiver.id,
                        sent: false,
                    });
                    for packet in result {
                        let _ = tx.blocking_send(packet);
                    }
                }
                tracing::debug!(target: "rvnic", "[{}]Recv batch {} @ {}", self_raw_fd, batch_count, loop_cnt);
            }
        }

        tracing::warn!("Rvnic recv thread for {} exited!", self_raw_fd);
    }
}
