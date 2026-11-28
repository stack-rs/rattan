use crate::ecn;
use std::fmt::Debug;

pub use etherparse::IpEcn;
use etherparse::{Ethernet2Header, Ipv4Header};
use tokio::time::{Duration, Instant};

use rattan_log::FlowDesc;

pub use crate::error::MetalError;
pub use crate::veth::VethCell;
pub use crate::{InterfaceBuildArtifact, InterfaceDriver, InterfaceReceiver, InterfaceSender};

pub enum PacketType {
    PacketHost = 0,
    _PacketBroadcast = 1,
    _PacketMulticast = 2,
    PacketOtherhost = 3,
    _PacketOutgoing = 4,
}

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

    fn ether_hdr(&self) -> Option<Ethernet2Header> {
        etherparse::Ethernet2Header::from_slice(self.as_slice()).map_or(None, |x| Some(x.0))
    }

    // TODO: remove this when 0.1.1 is released.
    // Timestamp
    /// Returns the timestamp at which this packet should have reached the cell
    ///
    /// This is initially set by rattan and needs to be updated when the packet leaves the cell by calling
    /// `delay_until` or `delay_by`.
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
    /// Like delay_until this should be the theoretical duration spent in the cell.
    /// This help to avoid over-delaying packets due to sleep time shift.
    fn delay_by(&mut self, delay: Duration);

    /// Sets the timestamp at which the packet should have left the cell, if the packet is delayed.
    ///
    /// Like `delay_by`this should be the theoretical duration spent in the cell.
    /// This helps to avoid over-delaying packets due to sleep time shift.
    fn delay_until(&mut self, timestamp: Instant);

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

    fn debug_log(&self, _args: std::fmt::Arguments) {
        let _ = _args;
    }
}

/// Finds the IP header in an untagged Ethernet II frame.
///
/// Returns `None` for unsupported EtherTypes or a missing or mismatched IP
/// version nibble. Accessors validate the header bytes they need.
pub fn ethernet_l3_offset(frame: &[u8]) -> Option<usize> {
    use etherparse::{EtherType, Ethernet2HeaderSlice};

    let ethernet = Ethernet2HeaderSlice::from_slice(frame).ok()?;
    let offset = Ethernet2Header::LEN;
    match (ethernet.ether_type(), *frame.get(offset)? >> 4) {
        (EtherType::IPV4, 4) | (EtherType::IPV6, 6) => Some(offset),
        _ => None,
    }
}

/// Access to a packet's IP header. Implement this trait explicitly for packet
/// types that expose an IP header, overriding the offset for other layouts.
pub trait PacketHeader: Packet {
    /// Byte offset of the IP header, shared by both buffer views.
    fn l3_offset(&self) -> Option<usize> {
        ethernet_l3_offset(self.as_slice())
    }

    /// Returns the IP header and remaining bytes.
    fn ip_slice(&self) -> Option<&[u8]> {
        self.as_slice().get(self.l3_offset()?..)
    }

    /// Returns the mutable IP header and remaining bytes.
    fn ip_slice_mut(&mut self) -> Option<&mut [u8]> {
        let offset = self.l3_offset()?;
        self.as_raw_buffer().get_mut(offset..)
    }

    /// Parses the IPv4 header, if present.
    fn ip_hdr(&self) -> Option<Ipv4Header> {
        Ipv4Header::from_slice(self.ip_slice()?)
            .ok()
            .map(|(header, _)| header)
    }

    /// Reads the ECN codepoint from an IPv4 or IPv6 header.
    fn ecn(&self) -> Option<IpEcn> {
        ecn::get_ecn(self.ip_slice()?)
    }

    /// Marks ECT(0) or ECT(1) as CE, updating the IPv4 header checksum.
    ///
    /// Returns `true` for a successful mark or an existing CE codepoint.
    /// Unsupported or Not-ECT packets return `false` and stay unchanged.
    fn set_ce(&mut self) -> bool {
        self.ip_slice_mut().is_some_and(ecn::set_ce)
    }
}
