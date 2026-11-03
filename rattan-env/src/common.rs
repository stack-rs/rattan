use std::fmt::Debug;

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

    fn ip_hdr(&self) -> Option<Ipv4Header> {
        if let Ok(result) = etherparse::Ethernet2Header::from_slice(self.as_slice()) {
            if let Ok(ip_hdr) = etherparse::Ipv4Header::from_slice(result.1) {
                return Some(ip_hdr.0);
            }
        }
        None
    }

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
}
