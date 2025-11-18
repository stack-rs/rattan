use crate::error::Error;
use async_trait::async_trait;
use etherparse::{Ethernet2Header, Ipv4Header, TcpOptionElement};
use rattan_log::FlowDesc;
#[cfg(feature = "serde")]
use serde::Deserialize;
use std::{fmt::Debug, sync::Arc};
use tokio::time::Instant;
pub mod bandwidth;
pub mod delay;
pub mod external;
pub mod loss;
pub mod per_packet;
pub mod router;
pub mod shadow;
pub mod spy;
pub mod token_bucket;

pub trait Packet: Debug + 'static + Send {
    type PacketGenerator;
    fn empty(maximum: usize, generator: &Self::PacketGenerator) -> Self;

    // fn empty(maximum: usize) -> Self;
    fn from_raw_buffer(buf: &[u8]) -> Self;

    // Raw buffer length
    fn length(&self) -> usize;
    // Link layer length, i.e. the length of the Ethernet frame (not including the preamble, SFD, FCS and IPG)
    fn l2_length(&self) -> usize;
    // Network layer length
    fn l3_length(&self) -> usize;
    fn as_slice(&self) -> &[u8];
    fn as_raw_buffer(&mut self) -> &mut [u8];
    fn ether_hdr(&self) -> Option<Ethernet2Header>;
    fn ip_hdr(&self) -> Option<Ipv4Header>;

    // Timestamp
    fn get_timestamp(&self) -> Instant;
    fn set_timestamp(&mut self, timestamp: Instant);

    // Packet description
    fn desc(&self) -> String {
        String::new()
    }

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

    fn from_raw_buffer(buf: &[u8]) -> Self {
        Self {
            buf: buf.to_vec(),
            timestamp: Instant::now(),
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

    fn set_timestamp(&mut self, timestamp: Instant) {
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
                                    // Check and record the window scale option, only if SYN bit
                                    // is set (SYN/ SYN_ACK) packet.
                                    let window_scale = tcp_hdr.syn().then(|| {
                                        tcp_hdr.options_iterator().find_map(|option| match option {
                                            Ok(TcpOptionElement::WindowScale(scale)) => Some(scale),
                                            _ => None,
                                        })
                                    });
                                    Some(FlowDesc::TCP(
                                        ip_hdr.source_addr(),
                                        ip_hdr.destination_addr(),
                                        tcp_hdr.source_port(),
                                        tcp_hdr.destination_port(),
                                        window_scale.flatten(),
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

    /// Set the notify receiver for the cell to handle Start signals internally
    fn set_notify_receiver(
        &mut self,
        _notify_rx: tokio::sync::broadcast::Receiver<crate::control::RattanNotify>,
    ) {
    }
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

/// Called at the start of the cell's dequeue() method. Make sure that no packets shall be dequeued
/// until an expected Notification has been received ONCE.
#[macro_export]
macro_rules! wait_until_started {
    ($self:ident, $variant:ident) => {
        while !$self.started {
            if let Some(notify_rx) = &mut $self.notify_rx {
                match notify_rx.recv().await {
                    Ok($crate::control::RattanNotify::$variant) => {
                        $self.reset();
                        $self.change_state(2);
                        $self.started = true;
                    }
                    Ok(_) => {
                        // Ignore unexpected notifications.
                        continue;
                    }
                    Err(_) => {
                        // This happens when the notifier is dropped.
                        return None;
                    }
                }
            } else {
                // The notifier is not set unless the normal startup of Rattan has taken place. In some
                // non-integrated environments, the notifier may not be set, like unit tests for cells.
                break;
            }
        }
    };
}

/// Cells that replay a trace should refer to TRACE_START_INSTANT as the logical start instant of the trace.
#[cfg(not(feature = "first-packet"))]
pub use crate::core::CALIBRATED_START_INSTANT as TRACE_START_INSTANT;
#[cfg(feature = "first-packet")]
pub use crate::core::FIRST_PACKET_INSTANT as TRACE_START_INSTANT;
