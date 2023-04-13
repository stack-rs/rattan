use async_trait::async_trait;
use std::fmt::Debug;

pub mod delay;

pub trait Packet: Debug {}

#[async_trait]
pub trait Device<P> {
    async fn enqueue(&mut self, packet: P) -> bool;
    async fn dequeue(&mut self) -> Option<P>;
}
