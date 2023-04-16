use async_trait::async_trait;
use etherparse::{Ethernet2Header, Ipv4Header};
use std::{fmt::Debug, sync::Arc};

use crate::error::Error;

pub mod delay;
pub mod external;

pub trait Packet: Debug + 'static + Send {
    fn empty(maximum: usize) -> Self;
    fn from_raw_buffer(buf: &[u8]) -> Self;
    fn length(&self) -> usize;
    fn as_raw_buffer(&mut self) -> &mut [u8];
    fn ether_hdr(&self) -> Option<Ethernet2Header>;
    fn ip_hdr(&self) -> Option<Ipv4Header>;
}

#[derive(Debug)]
pub struct StdPacket {
    buf: Vec<u8>,
}

impl Packet for StdPacket {
    fn empty(maximum: usize) -> Self {
        Self {
            buf: Vec::with_capacity(maximum),
        }
    }

    fn from_raw_buffer(buf: &[u8]) -> Self {
        Self { buf: buf.to_vec() }
    }

    fn length(&self) -> usize {
        self.buf.len()
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

pub trait ControlInterface {
    type Config;
    fn set_config(&mut self, config: Self::Config) -> Result<(), Error>;
}

#[async_trait]
pub trait Device<P>
where
    P: Packet,
{
    type IngressType: Ingress<P> + 'static;
    type EgressType: Egress<P> + 'static;
    type ControlInterfaceType: ControlInterface + 'static;

    fn sender(&self) -> Arc<Self::IngressType>;
    fn receiver(&mut self) -> &mut Self::EgressType;
    fn into_receiver(self) -> Self::EgressType;
    fn control_interface(&self) -> Arc<Self::ControlInterfaceType>;
}
