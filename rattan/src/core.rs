use std::{collections::HashMap, sync::Arc};

use tokio_util::sync::CancellationToken;

use crate::devices::{Device, Egress, Ingress, Packet};

pub struct RattanMachine<P>
where
    P: Packet,
{
    token: CancellationToken,
    sender: HashMap<usize, Arc<dyn Ingress<P>>>,
    receiver: HashMap<usize, Box<dyn Egress<P>>>,
    router: HashMap<usize, usize>,
}

impl<P> Default for RattanMachine<P>
where
    P: Packet + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<P> RattanMachine<P>
where
    P: Packet + 'static,
{
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            sender: HashMap::new(),
            receiver: HashMap::new(),
            router: HashMap::new(),
        }
    }

    pub fn add_device(&mut self, device: impl Device<P>) -> (usize, usize) {
        let tx_id = self.sender.len();
        self.sender.insert(tx_id, device.sender());
        let rx_id = self.receiver.len();
        self.receiver
            .insert(rx_id, Box::new(device.into_receiver()));
        (tx_id, rx_id)
    }

    pub fn link_device(&mut self, rx_id: usize, tx_id: usize) {
        self.router.insert(rx_id, tx_id);
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub async fn core_loop(&mut self) {
        let mut handles = Vec::new();
        for (&rx_id, &tx_id) in self.router.iter() {
            let mut rx = self.receiver.remove(&rx_id).unwrap();
            let tx = self.sender.get(&tx_id).unwrap().clone();
            let token_dup = self.token.clone();
            handles.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        packet = rx.dequeue() => {
                            if let Some(p) = packet {
                                // println!("forward a packet from {} to {}", rx_id, tx_id);
                                tx.enqueue(p).unwrap();
                            }
                        }
                        _ = token_dup.cancelled() => {
                            return
                        }
                    }
                }
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
    }
}
