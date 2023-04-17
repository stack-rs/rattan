use rattan::{
    core::RattanMachine,
    devices::{delay::DelayDevice, external::VirtualEthernet, StdPacket},
    env::get_std_env,
    metal::io::AfPacketDriver,
};

fn main() -> anyhow::Result<()> {
    let _std_env = get_std_env().unwrap();
    let mut machine = RattanMachine::<StdPacket>::new();
    let canncel_token = machine.cancel_token();

    ctrlc::set_handler(move || {
        canncel_token.cancel();
    })
    .unwrap();

    {
        _std_env.rattan_ns.enter().unwrap();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async move {
            let left_delay_device = DelayDevice::<StdPacket>::new();
            let right_delay_device = DelayDevice::<StdPacket>::new();
            let left_device =
                VirtualEthernet::<StdPacket, AfPacketDriver>::new(_std_env.left_pair.right.clone());
            let right_device =
                VirtualEthernet::<StdPacket, AfPacketDriver>::new(_std_env.right_pair.left.clone());

            let (left_delay_rx, left_delay_tx) = machine.add_device(left_delay_device);
            let (right_delay_rx, right_delay_tx) = machine.add_device(right_delay_device);
            let (left_device_rx, left_device_tx) = machine.add_device(left_device);
            let (right_device_rx, right_device_tx) = machine.add_device(right_device);

            machine.link_device(left_device_rx, left_delay_tx);
            machine.link_device(left_delay_rx, right_device_tx);
            machine.link_device(right_device_rx, right_delay_tx);
            machine.link_device(right_delay_rx, left_device_tx);

            machine.core_loop().await
        });
    }
    Ok(())
}