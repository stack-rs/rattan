use std::{collections::HashMap, sync::Arc};

use tokio_util::sync::CancellationToken;

use crate::{
    control::{http::HttpControlEndpoint, ControlEndpoint},
    devices::{Device, Egress, Ingress, Packet}, metal::netns::NetNs,
};

pub struct RattanMachine<P>
where
    P: Packet,
{
    token: CancellationToken,
    sender: HashMap<usize, Arc<dyn Ingress<P>>>,
    receiver: HashMap<usize, Box<dyn Egress<P>>>,
    router: HashMap<usize, usize>,
    config_endpoint: HttpControlEndpoint,
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
            config_endpoint: HttpControlEndpoint::new(),
        }
    }

    pub fn add_device(&mut self, device: impl Device<P>) -> (usize, usize) {
        let tx_id = self.sender.len();
        let rx_id = self.receiver.len();

        let control_interface = device.control_interface();
        self.config_endpoint
            .register_device(rx_id, control_interface);

        self.sender.insert(tx_id, device.sender());
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

    pub async fn core_loop(&mut self, original_ns: Arc<NetNs>) {
        let mut handles = Vec::new();
        let router_clone = self.config_endpoint.router();
        let token_dup = self.token.clone();

        let control_thread = std::thread::spawn(move || {
            original_ns.enter().unwrap();
            println!("control thread started");

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let server = axum::Server::bind(&"127.0.0.1:8080".parse().unwrap()).serve(router_clone.into_make_service()).with_graceful_shutdown(async {
                    token_dup.cancelled().await;
                });
                match server.await {
                    Ok(_) => {}
                    Err(e) => {
                        println!("server error: {}", e);
                    }
                }
            })
        });

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

        control_thread.join().unwrap();
    }
}
