use crate::{
    devices::{delay::DelayDevice, external::VirtualEthernet, Device, Egress, Ingress, StdPacket},
    env::StdNetEnv,
    metal::io::AfPacketDriver,
};

pub async fn core_loop(env: StdNetEnv) {
    println!("enter core loop");
    let mut delay_device_left: DelayDevice<StdPacket> = DelayDevice::new();
    let mut delay_device_right: DelayDevice<StdPacket> = DelayDevice::new();

    let mut left_device =
        VirtualEthernet::<StdPacket, AfPacketDriver>::new(env.left_pair.right.clone());
    let mut right_device =
        VirtualEthernet::<StdPacket, AfPacketDriver>::new(env.right_pair.left.clone());

    let mut handles = Vec::new();

    let left_device_tx = left_device.sender();
    let right_device_tx = right_device.sender();
    let delay_device_ltor_tx = delay_device_left.sender();
    let delay_device_rtol_tx = delay_device_right.sender();

    handles.push(tokio::spawn(async move {
        loop {
            if let Some(packet) = left_device.receiver().dequeue().await {
                println!("forward a packet from left device to delay device");
                delay_device_ltor_tx.enqueue(packet).unwrap();
            }
        }
    }));

    handles.push(tokio::spawn(async move {
        loop {
            if let Some(packet) = delay_device_left.receiver().dequeue().await {
                println!("forward a packet from delay device to right device");
                right_device_tx.enqueue(packet).unwrap();
            }
        }
    }));

    handles.push(tokio::spawn(async move {
        loop {
            if let Some(packet) = right_device.receiver().dequeue().await {
                println!("forward a packet from right device to delay device");
                delay_device_rtol_tx.enqueue(packet).unwrap();
            }
        }
    }));

    handles.push(tokio::spawn(async move {
        loop {
            if let Some(packet) = delay_device_right.receiver().dequeue().await {
                println!("forward a packet from delay device to left device");
                left_device_tx.enqueue(packet).unwrap();
            }
        }
    }));

    for handle in handles {
        handle.await.unwrap();
    }
}
