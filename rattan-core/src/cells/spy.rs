use crate::cells::{Cell, ControlInterface, Egress, Ingress, Packet};
use crate::error::Error;
use async_trait::async_trait;
use pcap_file::pcap::{PcapPacket, PcapWriter};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::io::Write;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tracing::{debug, info};

#[derive(Clone)]
pub struct SpyCellIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<(Duration, P)>,
}

pub struct SpyCell<P: Packet, W: Write> {
    ingress: Arc<SpyCellIngress<P>>,
    egress: SpyCellEgress<P, W>,
    control_interface: Arc<SpyCellControlInterface<W>>,
}

pub struct SpyCellEgress<P, W>
where
    P: Packet,
    W: Write,
{
    egress: mpsc::UnboundedReceiver<(Duration, P)>,
    config_rx: mpsc::UnboundedReceiver<SpyCellConfig<W>>,
    last: tokio::time::Instant,
    state: AtomicI32,
    spy: Option<PcapWriter<W>>,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone)]
pub struct SpyCellConfig<W> {
    #[cfg_attr(feature = "serde", serde(skip, default = "Option::default"))]
    pub spy: Option<W>,
}

pub struct SpyCellControlInterface<W> {
    config_tx: mpsc::UnboundedSender<SpyCellConfig<W>>,
}

impl<P, W> SpyCellEgress<P, W>
where
    P: Packet + Send + Sync,
    W: Write,
{
    fn set_config(&mut self, config: SpyCellConfig<W>) {
        debug!("Set inner spy");
        self.spy = config.spy.map(|spy| {
            PcapWriter::new(spy)
                .unwrap_or_else(|err| panic!("Failed to create the spy pcap writer: {err}"))
        });
    }
}

impl<W> SpyCellConfig<W> {
    pub fn new(spy: impl Into<W>) -> Self {
        Self {
            spy: Some(spy.into()),
        }
    }
}

impl<P, W> SpyCell<P, W>
where
    P: Packet,
    W: Write,
{
    pub fn new(spy: impl Into<W>) -> Result<SpyCell<P, W>, Error> {
        let (rx, tx) = mpsc::unbounded_channel();
        let (config_tx, config_rx) = mpsc::unbounded_channel();
        Ok(SpyCell {
            ingress: Arc::new(SpyCellIngress { ingress: rx }),
            egress: SpyCellEgress {
                egress: tx,
                config_rx,
                last: tokio::time::Instant::now(),
                state: AtomicI32::new(0),
                spy: Some(PcapWriter::new(spy.into())?),
            },
            control_interface: Arc::new(SpyCellControlInterface { config_tx }),
        })
    }
}

impl<P> Ingress<P> for SpyCellIngress<P>
where
    P: Packet + Send,
{
    fn enqueue(&self, mut packet: P) -> Result<(), Error> {
        packet.set_timestamp(tokio::time::Instant::now());
        self.ingress
            .send((
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .expect("Time went backwards"),
                packet,
            ))
            .map_err(|_| Error::ChannelError("Data channel is closed.".to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl<P, W> Egress<P> for SpyCellEgress<P, W>
where
    P: Packet + Send + Sync,
    W: Write + Send,
{
    async fn dequeue(&mut self) -> Option<P> {
        loop {
            tokio::select! {
            biased;
                Some(config) = self.config_rx.recv() => {
                    self.set_config(config);
                }
                Some((timestamp, packet)) = self.egress.recv() => {
                    match self.state.load(std::sync::atomic::Ordering::Acquire) {
                        0 => {
                            return None;
                        }
                        1 => {
                            return Some(packet);
                        }
                        _ => ()
                    }
                    assert!(self.last < packet.get_timestamp());
                    self.last = packet.get_timestamp();
                    self.spy.as_mut().map(|spy| spy.write_packet(&PcapPacket {
                            timestamp,
                            orig_len: packet.length() as u32,
                            data: packet.as_slice().into(),
                        }).expect("Failed to register a packet"));

                    return Some(packet)
                }
            }
        }
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }
}

impl<W: Send + 'static> ControlInterface for SpyCellControlInterface<W> {
    type Config = SpyCellConfig<W>;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        info!("Changing the spy");
        self.config_tx
            .send(config)
            .map_err(|_| Error::ConfigError("Control channel is closed.".to_string()))?;
        Ok(())
    }
}

impl<P, W> Cell<P> for SpyCell<P, W>
where
    P: Packet + Send + Sync + 'static,
    W: Write + Send + 'static,
{
    type IngressType = SpyCellIngress<P>;
    type EgressType = SpyCellEgress<P, W>;
    type ControlInterfaceType = SpyCellControlInterface<W>;

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
        Arc::clone(&self.control_interface)
    }
}

#[cfg(test)]
mod test {
    use std::fs::File;

    use pnet::{
        packet::{
            ethernet::{EtherType, MutableEthernetPacket},
            ip::IpNextHeaderProtocols,
            ipv4::MutableIpv4Packet,
            tcp::MutableTcpPacket,
        },
        util::MacAddr,
    };
    use tempfile::NamedTempFile;

    use crate::cells::StdPacket;

    use super::*;

    const ETH_HEADER_LEN: usize = 14;
    const IP_HEADER_LEN: usize = 20;
    const TCP_HEADER_LEN: usize = 20;
    const MAX_PAYLOAD_LEN: u64 =
        (u16::MAX as usize - (ETH_HEADER_LEN + IP_HEADER_LEN + TCP_HEADER_LEN)) as u64;

    fn spy_cell<P: Packet + Sync>() -> (NamedTempFile, SpyCell<P, File>) {
        let fifo = tempfile::NamedTempFile::with_suffix("spy_raw.pcap").unwrap();
        let file = File::create(fifo.path()).unwrap();
        let mut cell = SpyCell::new(file).unwrap();
        cell.receiver().change_state(2);
        (fifo, cell)
    }

    #[tokio::test]
    async fn test_spy_cell_ordering() {
        let (spy, mut cell) = spy_cell::<StdPacket>();

        let packet_creation = |id| -> Vec<u8> {
            let size = if id < MAX_PAYLOAD_LEN as u32 {
                id as u16
            } else {
                MAX_PAYLOAD_LEN as u16
            };
            let seq = if id == 0 {
                0
            } else if id < MAX_PAYLOAD_LEN as u32 {
                (id * (id - 1)) / 2 + 1
            } else {
                (MAX_PAYLOAD_LEN * (MAX_PAYLOAD_LEN + 1) / 2
                    + (id as u64 - MAX_PAYLOAD_LEN) * MAX_PAYLOAD_LEN) as u32
            };

            let mut buffer =
                vec![0; ETH_HEADER_LEN + IP_HEADER_LEN + TCP_HEADER_LEN + size as usize];

            // ETH header
            let mut eth_packet = MutableEthernetPacket::new(&mut buffer).unwrap();
            eth_packet.set_source(MacAddr::from([0x66; 6]));
            eth_packet.set_destination(MacAddr::from([0x22; 6]));
            eth_packet.set_ethertype(EtherType::new(0x0800));

            // IP header
            let mut ip_packet = MutableIpv4Packet::new(&mut buffer[ETH_HEADER_LEN..]).unwrap();
            ip_packet.set_version(4);
            ip_packet.set_header_length((IP_HEADER_LEN / 4) as u8);
            ip_packet.set_total_length(size + TCP_HEADER_LEN as u16 + IP_HEADER_LEN as u16);
            ip_packet.set_next_level_protocol(IpNextHeaderProtocols::Tcp);
            ip_packet.set_source([1, 1, 1, 1].into());
            ip_packet.set_ttl(64);
            ip_packet.set_destination([2, 2, 2, 2].into());

            // TCP header
            let mut tcp_packet =
                MutableTcpPacket::new(&mut buffer[ETH_HEADER_LEN + IP_HEADER_LEN..]).unwrap();
            tcp_packet.set_source(1);
            tcp_packet.set_destination(2);
            tcp_packet.set_sequence(seq);
            tcp_packet.set_acknowledgement(id);
            tcp_packet.set_flags(if id == 0 { 0b0000_0010 } else { 0b0001_0000 });
            tcp_packet.set_data_offset((TCP_HEADER_LEN / 4) as u8);
            tcp_packet.set_window(u16::MAX);

            // panic!("{buffer:?}");

            buffer
        };

        let packets = (0..100_000).map(packet_creation);

        let packets_clone = packets.clone();
        let sender = Arc::clone(&cell.sender());
        let server = tokio::task::spawn(async move {
            for packet in packets_clone {
                sender.enqueue(StdPacket::from_raw_buffer(&packet)).unwrap();
            }
        });

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let packets_clone = packets.clone();
        let client = tokio::spawn(async move {
            for packet in packets_clone {
                let new = cell.receiver().dequeue().await.unwrap();
                assert_eq!(new.as_slice(), packet);
            }
        });

        server.await.unwrap();
        client.await.unwrap();

        let mut last = core::time::Duration::ZERO;
        let mut spy = pcap_file::pcap::PcapReader::new(spy).unwrap();
        for packet in packets {
            let new = spy.next_packet().unwrap().unwrap();
            assert_eq!(new.data, packet);
            assert!(new.timestamp >= last, "{:?} < {last:?}", new.timestamp);
            last = new.timestamp;
        }
    }
}
