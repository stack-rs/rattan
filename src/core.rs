use tokio_util::sync::CancellationToken;

use crate::{
    devices::{delay::DelayDevice, external::VirtualEthernet, Device, Egress, Ingress, StdPacket},
    env::StdNetEnv,
    metal::io::AfPacketDriver,
};

pub struct RattanMachine {
    token: CancellationToken,
}

impl RattanMachine {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub async fn run(&mut self, env: StdNetEnv) {
        let mut handles = Vec::new();
        let mut left_delay_device = DelayDevice::new();
        let mut right_delay_device = DelayDevice::new();
        let mut left_device =
            VirtualEthernet::<StdPacket, AfPacketDriver>::new(env.left_pair.right.clone());
        let mut right_device =
            VirtualEthernet::<StdPacket, AfPacketDriver>::new(env.right_pair.left.clone());

        let left_device_tx = left_device.sender();
        let right_device_tx = right_device.sender();
        let delay_device_ltor_tx = left_delay_device.sender();
        let delay_device_rtol_tx = right_delay_device.sender();

        let token_dup = self.token.clone();
        handles.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    packet = left_device.receiver().dequeue() => {
                        if let Some(p) = packet {
                            println!("forward a packet from left device to delay device");
                            delay_device_ltor_tx.enqueue(p).unwrap();
                        }
                    }
                    _ = token_dup.cancelled() => {
                        return
                    }
                }
            }
        }));

        let token_dup = self.token.clone();
        handles.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    packet = left_delay_device.receiver().dequeue() => {
                        if let Some(p) = packet {
                            println!("forward a packet from delay device to right device");
                            right_device_tx.enqueue(p).unwrap();
                        }
                    }
                    _ = token_dup.cancelled() => {
                        return
                    }
                }
            }
        }));

        let token_dup = self.token.clone();
        handles.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    packet = right_device.receiver().dequeue() => {
                        if let Some(p) = packet {
                            println!("forward a packet from right device to delay device");
                            delay_device_rtol_tx.enqueue(p).unwrap();
                        }
                    }
                    _ = token_dup.cancelled() => {
                        return
                    }
                }
            }
        }));

        let token_dup = self.token.clone();
        handles.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    packet = right_delay_device.receiver().dequeue() => {
                        if let Some(p) = packet {
                            println!("forward a packet from delay device to left device");
                            left_device_tx.enqueue(p).unwrap();
                        }
                    }
                    _ = token_dup.cancelled() => {
                        return
                    }
                }
            }
        }));

        let token_dup = self.token.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.unwrap();
            token_dup.cancel();
        });

        for handle in handles {
            handle.await.unwrap();
        }
    }
}