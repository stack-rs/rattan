use std::{collections::HashMap, sync::Arc};

use tokio_util::sync::CancellationToken;
use tracing::{error, info, span, trace, Instrument, Level};

use crate::{
    devices::{Device, Egress, Ingress, Packet},
    metal::netns::NetNs,
};

#[cfg(feature = "http")]
use crate::control::{http::HttpControlEndpoint, ControlEndpoint};

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

#[derive(Debug)]
pub struct RattanMachineConfig {
    pub original_ns: Arc<NetNs>,
    pub port: u16,
}

pub struct RattanMachine<P>
where
    P: Packet,
{
    token: CancellationToken,
    sender: HashMap<usize, Arc<dyn Ingress<P>>>,
    receiver: HashMap<usize, Box<dyn Egress<P>>>,
    router: HashMap<usize, usize>,
    #[cfg(feature = "http")]
    config_endpoint: HttpControlEndpoint,
    #[cfg(feature = "packet-dump")]
    pcap_writer: Arc<Mutex<PcapNgWriter<Vec<u8>>>>,
}

impl<P> Default for RattanMachine<P>
where
    P: Packet + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<P> RattanMachine<P>
where
    P: Packet + 'static,
{
    pub fn new() -> Self {
        info!("New RattanMachine");
        Self {
            token: CancellationToken::new(),
            sender: HashMap::new(),
            receiver: HashMap::new(),
            router: HashMap::new(),
            #[cfg(feature = "http")]
            config_endpoint: HttpControlEndpoint::new(),
            #[cfg(feature = "packet-dump")]
            pcap_writer: Arc::new(Mutex::new(
                PcapNgWriter::new(Vec::new()).expect("Error creating pcapng writer"),
            )),
        }
    }

    pub fn add_device(&mut self, device: impl Device<P>) -> (usize, usize) {
        let tx_id = self.sender.len();
        let rx_id = self.receiver.len();

        #[cfg(feature = "http")]
        let control_interface = device.control_interface();
        #[cfg(feature = "http")]
        self.config_endpoint
            .register_device(rx_id, control_interface);

        self.sender.insert(tx_id, device.sender());
        self.receiver
            .insert(rx_id, Box::new(device.into_receiver()));

        #[cfg(feature = "packet-dump")]
        {
            let interface = InterfaceDescriptionBlock {
                linktype: pcap_file::DataLink::ETHERNET,
                snaplen: 0xFFFF,
                // XXX: Solve the problem <https://github.com/courvoif/pcap-file/pull/32>
                options: vec![InterfaceDescriptionOption::IfTsResol(9)],
            };
            self.pcap_writer
                .lock()
                .unwrap()
                .write_block(&pcap_file::pcapng::Block::InterfaceDescription(interface))
                .unwrap();
        }

        (tx_id, rx_id)
    }

    pub fn link_device(&mut self, rx_id: usize, tx_id: usize) {
        info!(rx_id, tx_id, "Link device:");
        self.router.insert(rx_id, tx_id);
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub async fn core_loop(&mut self, config: RattanMachineConfig) {
        let mut handles = Vec::new();
        #[cfg(feature = "control")]
        let router_clone = self.config_endpoint.router();
        #[cfg(feature = "control")]
        let token_dup = self.token.clone();
        #[cfg(feature = "control")]
        let control_thread_span = span!(Level::DEBUG, "control_thread").or_current();

        #[cfg(feature = "control")]
        let control_thread = std::thread::spawn(move || {
            let _entered = control_thread_span.entered();
            config.original_ns.enter().unwrap();
            info!("control thread started");

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                #[cfg(feature = "http")]
                let server =
                    axum::Server::bind(&format!("127.0.0.1:{}", config.port).parse().unwrap())
                        .serve(router_clone.into_make_service())
                        .with_graceful_shutdown(async {
                            token_dup.cancelled().await;
                        });
                #[cfg(feature = "http")]
                match server.await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("server error: {}", e);
                    }
                }
            })
        });

        for (&rx_id, &tx_id) in self.router.iter() {
            let mut rx = self.receiver.remove(&rx_id).unwrap();
            let tx = self.sender.get(&tx_id).unwrap().clone();
            let token_dup = self.token.clone();
            #[cfg(feature = "packet-dump")]
            let pcap_writer = self.pcap_writer.clone();

            handles.push(tokio::spawn(
                async move {
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
                                            interface_id: rx_id as u32,
                                            timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap(),
                                            original_len: p.length() as u32,
                                            data: std::borrow::Cow::Borrowed(p.as_slice()),
                                            options: vec![],
                                        };
                                        pcap_writer.lock().unwrap().write_block(&pcap_file::pcapng::Block::EnhancedPacket(packet_block)).unwrap();
                                    }
                                    tx.enqueue(p).unwrap();
                                }
                            }
                            _ = token_dup.cancelled() => {
                                return
                            }
                        }
                        tokio::task::yield_now().await;
                    }
                }
                .instrument(span!(Level::DEBUG, "router")),
            ));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        #[cfg(feature = "packet-dump")]
        if let Ok(path) = std::env::var("PACKET_DUMP_FILE") {
            let mut file_out = File::create(&path)
                .unwrap_or_else(|_| panic!("Error creating packet dump file {}", &path));
            file_out
                .write_all(self.pcap_writer.lock().unwrap().get_mut())
                .unwrap();
        };

        #[cfg(feature = "control")]
        control_thread.join().unwrap();
    }
}
