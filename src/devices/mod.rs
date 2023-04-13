use async_trait::async_trait;
use std::fmt::Debug;

pub mod delay;

pub trait Packet: Debug {
    fn empty(maximum: usize) -> Self;
    fn from_raw_buffer(buf: &[u8]) -> Self;
    fn length(&self) -> usize;
    fn as_raw_buffer<'a>(&self) -> &'a [u8];
    fn ether_hdr(&mut self);
}

#[async_trait]
pub trait Device<P> {
    fn enqueue(&mut self, packet: P) -> bool;
    async fn dequeue(&mut self) -> Option<P>;
}
