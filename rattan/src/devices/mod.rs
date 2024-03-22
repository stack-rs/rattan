use async_trait::async_trait;
use etherparse::{Ethernet2Header, Ipv4Header};
#[cfg(feature = "serde")]
use serde::Deserialize;
use std::{fmt::Debug, sync::Arc};
use tokio::time::Instant;

use crate::error::Error;

pub mod bandwidth;
pub mod delay;
pub mod external;
pub mod loss;

pub trait Packet: Debug + 'static + Send {
    fn empty(maximum: usize) -> Self;
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

    fn get_timestamp(&self) -> Instant;
    fn set_timestamp(&mut self, timestamp: Instant);
}

#[derive(Debug)]
pub struct StdPacket {
    buf: Vec<u8>,
    timestamp: Instant,
}

impl Packet for StdPacket {
    fn empty(maximum: usize) -> Self {
        Self {
            buf: Vec::with_capacity(maximum),
            timestamp: Instant::now(),
        }
    }

    fn from_raw_buffer(buf: &[u8]) -> Self {
        Self {
            buf: buf.to_vec(),
            timestamp: Instant::now(),
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
}

pub trait Ingress<P>: Send + Sync
where
    P: Packet,
{
    fn enqueue(&self, packet: P) -> Result<(), Error>;
}

#[async_trait]
pub trait Egress<P>: Send
where
    P: Packet,
{
    async fn dequeue(&mut self) -> Option<P>;
}

pub trait ControlInterface: Send + Sync + 'static {
    #[cfg(feature = "serde")]
    type Config: for<'a> Deserialize<'a> + Send;
    #[cfg(not(feature = "serde"))]
    type Config: Send;
    fn set_config(&self, config: Self::Config) -> Result<(), Error>;
}

#[async_trait]
pub trait Device<P>
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
