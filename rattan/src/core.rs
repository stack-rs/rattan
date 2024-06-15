use std::{
    collections::HashMap,
    fmt::Debug,
    io::ErrorKind,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};

use tokio::{
    runtime::{Handle, Runtime},
    sync::{broadcast, mpsc},
    task,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, span, trace, warn, Instrument, Level};

use crate::{
    control::{RattanController, RattanNotify, RattanOp, RattanOpEndpoint, RattanOpResult},
    devices::{Device, Egress, Ingress, Packet},
    error::{Error, RattanCoreError},
    metal::io::common::InterfaceDriver,
};

#[cfg(feature = "packet-dump")]
use pcap_file::pcapng::{
    blocks::{
        enhanced_packet::EnhancedPacketBlock,
        interface_description::{InterfaceDescriptionBlock, InterfaceDescriptionOption},
    },
    PcapNgWriter,
};
#[cfg(feature = "packet-dump")]
use std::{fs::File, io::Write, sync::Mutex};

pub trait DeviceFactory<D>: FnOnce(&Handle) -> Result<D, Error> {}

impl<T: FnOnce(&Handle) -> Result<D, Error>, D> DeviceFactory<D> for T {}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone)]
#[repr(u8)]
pub enum RattanState {
    Initial = 0,
    Spawned = 1,
    Running = 2,
    Exited = 3,
}

impl From<u8> for RattanState {
    fn from(v: u8) -> Self {
        match v {
            0 => RattanState::Initial,
            1 => RattanState::Spawned,
            2 => RattanState::Running,
            3 => RattanState::Exited,
            _ => panic!("Invalid RattanState value: {}", v),
        }
    }
}

pub struct RattanCore<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + 'static,
{
    // Env
    runtime: Arc<Runtime>,
    cancel_token: CancellationToken,
    rt_cancel_token: CancellationToken,
    op_endpoint: RattanOpEndpoint,

    // Build
    sender: HashMap<String, Arc<dyn Ingress<D::Packet>>>,
    receiver: HashMap<String, Box<dyn Egress<D::Packet>>>,
    router: HashMap<String, String>,

    // Runtime
    state: Arc<AtomicU8>, // RattanState
    rattan_handles: Vec<task::JoinHandle<()>>,
    rattan_notify_rx: broadcast::Receiver<RattanNotify>,
    controller_handle: task::JoinHandle<()>,

    #[cfg(feature = "packet-dump")]
    interface_id: HashMap<String, u32>,
    #[cfg(feature = "packet-dump")]
    pcap_writer: Arc<Mutex<PcapNgWriter<Vec<u8>>>>,
}

impl<D> RattanCore<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + 'static,
{
    pub fn new(
        runtime: Arc<Runtime>,
        cancel_token: CancellationToken,
        rt_cancel_token: CancellationToken,
    ) -> Self {
        info!("New RattanCore");
        let (notify_tx, notify_rx) = broadcast::channel(16);
        let (op_tx, op_rx) = mpsc::unbounded_channel();
        let state = Arc::new(AtomicU8::from(RattanState::Initial as u8));
        let controller_handle = runtime.spawn(
            RattanController::new(op_rx, notify_tx, cancel_token.child_token(), state.clone())
                .run(),
        );
        Self {
            runtime,
            rt_cancel_token,
            cancel_token,
            op_endpoint: RattanOpEndpoint::new(op_tx),

            sender: HashMap::new(),
            receiver: HashMap::new(),
            router: HashMap::new(),

            state,
            rattan_handles: Vec::new(),
            rattan_notify_rx: notify_rx,
            controller_handle,

            #[cfg(feature = "packet-dump")]
            interface_id: HashMap::new(),
            #[cfg(feature = "packet-dump")]
            pcap_writer: Arc::new(Mutex::new(
                PcapNgWriter::new(Vec::new()).expect("Error creating pcapng writer"),
            )),
        }
    }

    pub fn op_endpoint(&self) -> RattanOpEndpoint {
        self.op_endpoint.clone()
    }

    pub fn op_block_exec(&self, op: RattanOp) -> Result<RattanOpResult, Error> {
        self.runtime.block_on(self.op_endpoint.exec(op))
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
        info!("Build device \"{}\"", id);
        let device = builder(self.runtime.handle())?;
        let control_interface = device.control_interface();
        self.register_device(id.clone(), device)?;
        #[cfg(feature = "serde")]
        self.op_block_exec(RattanOp::AddControlInterface(
            id.clone(),
            control_interface.clone(),
        ))?;
        Ok(control_interface)
    }

    fn register_device(
        &mut self,
        id: String,
        device: impl Device<D::Packet>,
    ) -> Result<(), RattanCoreError> {
        match self.sender.entry(id.clone()) {
            std::collections::hash_map::Entry::Occupied(_) => {
                return Err(RattanCoreError::AddDeviceError(format!(
                    "Device with ID {} already exists in sender list",
                    id
                )));
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(device.sender());
            }
        };
        match self.receiver.entry(id.clone()) {
            std::collections::hash_map::Entry::Occupied(_) => {
                return Err(RattanCoreError::AddDeviceError(format!(
                    "Device with ID {} already exists in receiver list",
                    id
                )));
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Box::new(device.into_receiver()));
            }
        };

        #[cfg(feature = "packet-dump")]
        {
            let interface = InterfaceDescriptionBlock {
                linktype: pcap_file::DataLink::ETHERNET,
                snaplen: 0xFFFF,
                // XXX: Solve the problem <https://github.com/courvoif/pcap-file/pull/32>
                options: vec![
                    InterfaceDescriptionOption::IfName(std::borrow::Cow::Borrowed(&id)),
                    InterfaceDescriptionOption::IfTsResol(9),
                ],
            };
            self.interface_id
                .insert(id.clone(), self.interface_id.len() as u32);
            self.pcap_writer
                .lock()
                .unwrap()
                .write_block(&pcap_file::pcapng::Block::InterfaceDescription(interface))
                .unwrap();
        }

        Ok(())
    }

    pub fn link_device(&mut self, rx_id: String, tx_id: String) {
        info!("Link device \"{}\" --> \"{}\"", rx_id, tx_id);
        // Check existence of devices
        if !self.receiver.contains_key(&rx_id) {
            warn!("Device with ID {} does not exist in devices map", rx_id);
        }
        if !self.receiver.contains_key(&tx_id) {
            warn!("Device with ID {} does not exist in devices map", tx_id);
        }
        self.router.insert(rx_id, tx_id);
    }

    pub fn spawn_rattan(&mut self) -> Result<(), RattanCoreError> {
        if self.state.load(Ordering::Relaxed) != RattanState::Initial as u8 {
            return Err(RattanCoreError::SpawnError(format!(
                "Rattan state is {:?} instead of Initial",
                RattanState::from(self.state.load(Ordering::Relaxed))
            )));
        }
        if self.router.is_empty() {
            return Err(RattanCoreError::SpawnError(
                "No links specified".to_string(),
            ));
        }

        let rattan_cancel_token = self.cancel_token.child_token();
        for (rx_id, tx_id) in self.router.iter() {
            let rattan_cancel_token = rattan_cancel_token.clone();
            let mut notify_rx = self.rattan_notify_rx.resubscribe();
            let rx_id = rx_id.clone();
            let tx_id = tx_id.clone();
            let mut rx = self
                .receiver
                .remove(&rx_id)
                .ok_or_else(|| RattanCoreError::UnknownIdError(rx_id.clone()))?;
            let tx = self
                .sender
                .get(&tx_id)
                .ok_or_else(|| RattanCoreError::UnknownIdError(tx_id.clone()))?
                .clone();
            #[cfg(feature = "packet-dump")]
            let pcap_writer = self.pcap_writer.clone();
            #[cfg(feature = "packet-dump")]
            let interface_id = *self
                .interface_id
                .get(&rx_id)
                .ok_or_else(|| RattanCoreError::UnknownIdError(rx_id.clone()))?;

            self.rattan_handles.push(self.runtime.spawn(
                async move {
                    loop {
                        tokio::select! {
                            biased;
                            notify = notify_rx.recv() => {
                                match notify {
                                    Ok(RattanNotify::Start) => {
                                        rx.reset(); // Reset the device
                                        rx.change_state(2);
                                        break;
                                    }
                                    Err(_) => {
                                        warn!(rx_id, tx_id, "Core router exited since notify channel is closed before rattan start");
                                        return
                                    }
                                }
                            }
                            _ = rx.dequeue(), if rx_id == "left" || rx_id == "right" => {
                                debug!(rx_id, tx_id, "Drop packet since the rattan is not started yet");
                            }
                        }
                        tokio::task::yield_now().await;
                    }
                    loop {
                        tokio::select! {
                            packet = rx.dequeue() => {
                                if let Some(p) = packet {
                                    trace!(
                                        header = ?format!("{:X?}", &p.as_slice()[0..std::cmp::min(56, p.length())]),
                                        "forward from {} to {}",
                                        rx_id,
                                        tx_id,
                                    );
                                    #[cfg(feature = "packet-dump")]
                                    {
                                        let packet_block = EnhancedPacketBlock {
                                            interface_id,
                                            timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap(),
                                            original_len: p.length() as u32,
                                            data: std::borrow::Cow::Borrowed(p.as_slice()),
                                            options: vec![],
                                        };
                                        pcap_writer.lock().unwrap().write_block(&pcap_file::pcapng::Block::EnhancedPacket(packet_block)).unwrap();
                                    }
                                    match tx.enqueue(p) {
                                        Ok(_) => {}
                                        Err(Error::ChannelError(_)) => {
                                            rattan_cancel_token.cancel();
                                            info!(rx_id, tx_id, "Core router exited since the channel is closed");
                                            return
                                        }
                                        Err(Error::IoError(e)) if e.kind() == ErrorKind::WouldBlock => {
                                            debug!(rx_id, tx_id, "Drop packet since the system is busy");
                                        }
                                        Err(e) => {
                                            panic!("Error forwarding packet between {} and {}: {:?}", rx_id, tx_id, e);
                                        }
                                    }
                                }
                            }
                            _ = rattan_cancel_token.cancelled() => {
                                debug!(rx_id, tx_id, "Core router cancelled");
                                return
                            }
                        }
                        tokio::task::yield_now().await;
                    }
                }
                .instrument(span!(Level::DEBUG, "CoreRouter").or_current()),
            ));
        }

        self.state
            .store(RattanState::Spawned as u8, Ordering::Relaxed);
        Ok(())
    }

    pub fn send_notify(&mut self, notify: RattanNotify) -> Result<(), Error> {
        self.op_block_exec(RattanOp::SendNotify(notify)).map(|_| ())
    }

    pub fn start_rattan(&mut self) -> Result<(), Error> {
        self.send_notify(RattanNotify::Start)
    }

    pub fn join_rattan(&mut self) {
        if self.state.load(Ordering::Relaxed) == RattanState::Exited as u8 {
            return;
        }
        self.runtime.block_on(async {
            for handle in self.rattan_handles.drain(..) {
                handle.await.unwrap();
            }
        });
        self.rt_cancel_token.cancel();
        self.state
            .store(RattanState::Exited as u8, Ordering::Relaxed);
        #[cfg(feature = "packet-dump")]
        if let Ok(path) = std::env::var("PACKET_DUMP_FILE") {
            let mut file_out = File::create(&path)
                .unwrap_or_else(|_| panic!("Error creating packet dump file {}", &path));
            file_out
                .write_all(self.pcap_writer.lock().unwrap().get_mut())
                .unwrap();
        };
    }

    pub fn cancel_rattan(&mut self) {
        self.cancel_token.cancel();
        self.join_rattan();
        self.controller_handle.abort();
    }
}

impl<D> Drop for RattanCore<D>
where
    D: InterfaceDriver,
    D::Packet: Packet + 'static,
{
    fn drop(&mut self) {
        self.cancel_rattan();
    }
}
