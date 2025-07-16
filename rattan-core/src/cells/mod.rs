use async_trait::async_trait;
use etherparse::{Ethernet2Header, Ipv4Header};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::{fmt::Debug, net::Ipv4Addr, sync::Arc};
use tokio::time::{Duration, Instant};

use crate::error::Error;

pub mod bandwidth;
pub mod delay;
pub mod external;
pub mod loss;
pub mod per_packet;
pub mod router;
pub mod shadow;
pub mod spy;
pub mod token_bucket;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum FlowDesc {
    TCP(Ipv4Addr, Ipv4Addr, u16, u16),
}

pub trait Packet: Debug + 'static + Send {
    type PacketGenerator;
    fn empty(maximum: usize, generator: &Self::PacketGenerator) -> Self;

    // fn empty(maximum: usize) -> Self;
    /// Creates a new packet from its content and the timestamp at which it arrived
    fn from_raw_buffer(buf: &[u8], timestamp: Instant) -> Self;

    /// Returns the length of the raw buffer
    fn length(&self) -> usize;
    /// Returns the link layer length, i.e. the length of the Ethernet frame (not including the preamble, SFD, FCS and IPG)
    fn l2_length(&self) -> usize;
    /// Returns the network layer length
    fn l3_length(&self) -> usize;
    /// Returns a reference to the raw buffer
    fn as_slice(&self) -> &[u8];
    /// Returns a mutable reference to the raw buffer
    fn as_raw_buffer(&mut self) -> &mut [u8];
    /// Returns the Ethernet header, if any
    fn ether_hdr(&self) -> Option<Ethernet2Header>;
    /// Returns the IP header, if any
    fn ip_hdr(&self) -> Option<Ipv4Header>;

    // Timestamp
    /// Returns the timestamp at which this packet should have reached the cell
    ///
    /// This is initially set by rattan and need to be updated when leaving the cell with [`delay_until`](Self::delay_until) and [`delay_by`](Self::delay_by)
    fn get_timestamp(&self) -> Instant;
    /// Sets the timestamp of the packet
    #[deprecated(
        since = "0.1.0",
        note = "Use [`delay_until`](Self::delay_until) and [`delay_by`](Self::delay_by) instead"
    )]
    fn set_timestamp(&mut self, timestamp: Instant) {
        self.delay_until(timestamp);
    }

    /// Sets the duration the packet should have been delayed by the cell
    ///
    /// Like [`delay_until`](Self::delay_until) this should be the theoretical duration spent in the cell.
    /// This help to avoid over-delaying packets du to sleep timeshift.
    fn delay_by(&mut self, delay: Duration);

    /// Sets the timestamp at which the packet should have left the cell, if it delayed it
    ///
    /// Like [`delay_by`](Self::delay_by) this should be the theoretical duration spent in the cell.
    /// This help to avoid over-delaying packets du to sleep timeshift.
    fn delay_until(&mut self, timestamp: Instant);

    /// Returns a text description of the packet
    fn desc(&self) -> String {
        String::new()
    }

    /// Returns a packet description
    fn flow_desc(&self) -> Option<FlowDesc> {
        None
    }

    fn set_flow_id(&mut self, _flow_id: u32) {}
    fn get_flow_id(&self) -> u32 {
        0
    }
}

#[derive(Clone, Debug)]
pub struct StdPacket {
    buf: Vec<u8>,
    timestamp: Instant,
    flow_id: u32,
}

impl Packet for StdPacket {
    type PacketGenerator = ();

    fn empty(maximum: usize, _generator: &Self::PacketGenerator) -> Self {
        Self {
            buf: Vec::with_capacity(maximum),
            timestamp: Instant::now(),
            flow_id: 0,
        }
    }

    fn from_raw_buffer(buf: &[u8], timestamp: Instant) -> Self {
        Self {
            buf: buf.to_vec(),
            timestamp,
            flow_id: 0,
        }
    }

    fn length(&self) -> usize {
        self.buf.len()
    }

    fn l2_length(&self) -> usize {
        self.buf.len()
    }

    fn l3_length(&self) -> usize {
        // 14 is the length of the Ethernet header
        self.buf.len() - 14
    }

    fn as_slice(&self) -> &[u8] {
        self.buf.as_slice()
    }

    fn as_raw_buffer(&mut self) -> &mut [u8] {
        self.buf.as_mut_slice()
    }

    fn ip_hdr(&self) -> Option<Ipv4Header> {
        if let Ok(result) = etherparse::Ethernet2Header::from_slice(self.buf.as_slice()) {
            if let Ok(ip_hdr) = etherparse::Ipv4Header::from_slice(result.1) {
                return Some(ip_hdr.0);
            }
        }
        None
    }

    fn ether_hdr(&self) -> Option<Ethernet2Header> {
        etherparse::Ethernet2Header::from_slice(self.buf.as_slice()).map_or(None, |x| Some(x.0))
    }

    fn get_timestamp(&self) -> Instant {
        self.timestamp
    }

    fn delay_by(&mut self, delay: Duration) {
        self.timestamp += delay;
    }

    fn delay_until(&mut self, timestamp: Instant) {
        self.timestamp = timestamp;
    }

    fn desc(&self) -> String {
        let mut desc = String::new();
        if let Ok(ether_hdr) = etherparse::Ethernet2HeaderSlice::from_slice(self.buf.as_slice()) {
            desc.push_str("[Ether] ");
            match ether_hdr.ether_type() {
                etherparse::EtherType::ARP => desc.push_str("[ARP]"),
                etherparse::EtherType::IPV4 => {
                    desc.push_str("[IPv4]");
                    match etherparse::Ipv4HeaderSlice::from_slice(
                        self.buf
                            .as_slice()
                            .get(ether_hdr.slice().len()..)
                            .unwrap_or(&[]),
                    ) {
                        Ok(ip_hdr) => {
                            desc.push_str(&format!(
                                " src: {} dst: {} id: {} offset: {} chksum: {} ",
                                ip_hdr.source_addr(),
                                ip_hdr.destination_addr(),
                                ip_hdr.identification(),
                                ip_hdr.fragments_offset(),
                                ip_hdr.header_checksum()
                            ));
                            match ip_hdr.protocol() {
                                etherparse::IpNumber::UDP => {
                                    desc.push_str("[UDP]");
                                    if let Ok(udp_hdr) = etherparse::UdpHeaderSlice::from_slice(
                                        self.buf
                                            .as_slice()
                                            .get(ether_hdr.slice().len() + ip_hdr.slice().len()..)
                                            .unwrap_or(&[]),
                                    ) {
                                        desc.push_str(&format!(
                                            " sport: {} dport: {} chksum: {}",
                                            udp_hdr.source_port(),
                                            udp_hdr.destination_port(),
                                            udp_hdr.checksum()
                                        ));
                                    }
                                }
                                etherparse::IpNumber::TCP => {
                                    desc.push_str("[TCP]");
                                    if let Ok(tcp_hdr) = etherparse::TcpHeaderSlice::from_slice(
                                        self.buf
                                            .as_slice()
                                            .get(ether_hdr.slice().len() + ip_hdr.slice().len()..)
                                            .unwrap_or(&[]),
                                    ) {
                                        desc.push_str(&format!(
                                            " sport: {} dport: {} chksum: {} seq: {} ack: {} flags: {}",
                                            tcp_hdr.source_port(),
                                            tcp_hdr.destination_port(),
                                            tcp_hdr.checksum(),
                                            tcp_hdr.sequence_number(),
                                            tcp_hdr.acknowledgment_number(),
                                            tcp_hdr.slice().get(13).unwrap()
                                        ));
                                    }
                                }
                                etherparse::IpNumber::ICMP => desc.push_str("[ICMP]"),
                                etherparse::IpNumber::IPV6_ICMP => desc.push_str("[IPV6_ICMP]"),
                                _ => desc.push_str("[Unknown]"),
                            }
                        }
                        Err(e) => {
                            desc.push_str(&format!("Error parsing: {e}"));
                        }
                    }
                }

                etherparse::EtherType::IPV6 => desc.push_str("[IPv6]"),
                _ => desc.push_str("[Unknown]"),
            }
        } else {
            desc.push_str("[Unknown]");
        }
        desc
    }

    fn flow_desc(&self) -> Option<FlowDesc> {
        if let Ok(ether_hdr) = etherparse::Ethernet2HeaderSlice::from_slice(self.buf.as_slice()) {
            match ether_hdr.ether_type() {
                etherparse::EtherType::IPV4 => {
                    match etherparse::Ipv4HeaderSlice::from_slice(
                        self.buf
                            .as_slice()
                            .get(ether_hdr.slice().len()..)
                            .unwrap_or(&[]),
                    ) {
                        Ok(ip_hdr) => match ip_hdr.protocol() {
                            etherparse::IpNumber::TCP => {
                                if let Ok(tcp_hdr) = etherparse::TcpHeaderSlice::from_slice(
                                    self.buf
                                        .as_slice()
                                        .get(ether_hdr.slice().len() + ip_hdr.slice().len()..)
                                        .unwrap_or(&[]),
                                ) {
                                    Some(FlowDesc::TCP(
                                        ip_hdr.source_addr(),
                                        ip_hdr.destination_addr(),
                                        tcp_hdr.source_port(),
                                        tcp_hdr.destination_port(),
                                    ))
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        },
                        Err(_) => None,
                    }
                }
                _ => None,
            }
        } else {
            None
        }
    }

    fn set_flow_id(&mut self, flow_id: u32) {
        self.flow_id = flow_id;
    }

    fn get_flow_id(&self) -> u32 {
        self.flow_id
    }
}

#[cfg(test)]
#[derive(Clone, Debug, derive_more::Deref, derive_more::DerefMut)]
pub struct TestPacket<P> {
    init_timestamp: Instant,
    #[deref]
    #[deref_mut]
    packet: P,
}

#[cfg(test)]
impl<P: Packet> Packet for TestPacket<P> {
    type PacketGenerator = P::PacketGenerator;

    fn empty(maximum: usize, generator: &Self::PacketGenerator) -> Self {
        let packet = P::empty(maximum, generator);
        TestPacket {
            init_timestamp: packet.get_timestamp(),
            packet,
        }
    }

    fn from_raw_buffer(buf: &[u8], timestamp: Instant) -> Self {
        Self {
            init_timestamp: timestamp,
            packet: P::from_raw_buffer(buf, timestamp),
        }
    }

    fn length(&self) -> usize {
        self.packet.length()
    }

    fn l2_length(&self) -> usize {
        self.packet.l2_length()
    }

    fn l3_length(&self) -> usize {
        self.packet.l3_length()
    }

    fn as_slice(&self) -> &[u8] {
        self.packet.as_slice()
    }

    fn as_raw_buffer(&mut self) -> &mut [u8] {
        self.packet.as_raw_buffer()
    }

    fn ip_hdr(&self) -> Option<Ipv4Header> {
        self.packet.ip_hdr()
    }

    fn ether_hdr(&self) -> Option<Ethernet2Header> {
        self.packet.ether_hdr()
    }

    fn get_timestamp(&self) -> Instant {
        self.packet.get_timestamp()
    }

    fn delay_by(&mut self, delay: Duration) {
        self.packet.delay_by(delay)
    }

    fn delay_until(&mut self, timestamp: Instant) {
        self.packet.delay_until(timestamp)
    }

    fn desc(&self) -> String {
        self.packet.desc()
    }

    fn flow_desc(&self) -> Option<FlowDesc> {
        self.packet.flow_desc()
    }

    fn set_flow_id(&mut self, flow_id: u32) {
        self.packet.set_flow_id(flow_id);
    }

    fn get_flow_id(&self) -> u32 {
        self.packet.get_flow_id()
    }
}

#[cfg(test)]
impl<P: Packet> TestPacket<P> {
    pub fn delay(&self) -> Duration {
        self.get_timestamp() - self.init_timestamp
    }
}

pub trait Ingress<P>: Send + Sync
where
    P: Packet,
{
    fn enqueue(&self, packet: P) -> Result<(), Error>;

    fn reset(&mut self) {}
}

#[async_trait]
pub trait Egress<P>: Send
where
    P: Packet,
{
    async fn dequeue(&mut self) -> Option<P>;

    fn reset(&mut self) {}

    /// 0 means drop, 1 means pass-through, 2 means normal operation
    fn change_state(&self, _state: i32) {}
}

pub trait ControlInterface: Send + Sync + 'static {
    #[cfg(feature = "serde")]
    type Config: for<'a> Deserialize<'a> + Send;
    #[cfg(not(feature = "serde"))]
    type Config: Send;
    fn set_config(&self, config: Self::Config) -> Result<(), Error>;
}

#[cfg(feature = "serde")]
pub trait JsonControlInterface: Send + Sync {
    fn config_cell(&self, payload: serde_json::Value) -> Result<(), Error>;
}

#[cfg(feature = "serde")]
impl<T> JsonControlInterface for T
where
    T: ControlInterface,
{
    fn config_cell(&self, payload: serde_json::Value) -> Result<(), Error> {
        match serde_json::from_value(payload) {
            Ok(payload) => self.set_config(payload),
            Err(e) => Err(Error::ConfigError(e.to_string())),
        }
    }
}

#[async_trait]
pub trait Cell<P>
where
    P: Packet,
{
    type IngressType: Ingress<P> + 'static;
    type EgressType: Egress<P> + 'static;
    type ControlInterfaceType: ControlInterface;

    fn sender(&self) -> Arc<Self::IngressType>;
    fn receiver(&mut self) -> &mut Self::EgressType;
    fn into_receiver(self) -> Self::EgressType;
    fn control_interface(&self) -> Arc<Self::ControlInterfaceType>;
}
