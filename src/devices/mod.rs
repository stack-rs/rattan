use async_trait::async_trait;
use etherparse::{Ethernet2Header, Ipv4Header};
use std::fmt::Debug;

use crate::error::Error;

pub mod delay;
pub mod external;

pub trait Packet: Debug {
    fn empty(maximum: usize) -> Self;
    fn from_raw_buffer(buf: &[u8]) -> Self;
    fn length(&self) -> usize;
    fn as_raw_buffer<'a>(&self) -> &'a mut [u8];
    fn ether_hdr(&self) -> &Ethernet2Header;
    fn ip_hdr(&self) -> Option<&Ipv4Header>;
}

#[async_trait]
pub trait Device<P> {
    fn enqueue(&mut self, packet: P) -> Result<(), Error>;
    async fn dequeue(&mut self) -> Option<P>;
}
