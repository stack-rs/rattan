use rattan::{
    core::{RattanMachine, RattanMachineConfig},
    devices::{delay::DelayDevice, external::VirtualEthernet, StdPacket},
    env::{get_std_env, StdNetEnvConfig},
    metal::io::AfPacketDriver,
};

fn main() -> anyhow::Result<()> {
    let _std_env = get_std_env(StdNetEnvConfig::default()).unwrap();
    let mut machine = RattanMachine::<StdPacket>::new();
    let canncel_token = machine.cancel_token();

    ctrlc::set_handler(move || {
        canncel_token.cancel();
    })
    .unwrap();

    {
        let original_ns = _std_env.rattan_ns.enter().unwrap();
        let _left_pair_guard = _std_env.left_pair.clone();
        let _right_pair_guard = _std_env.right_pair.clone();

        let runtime = tokio::runtime::Builder::new_current_thread()
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

            let config = RattanMachineConfig {
                original_ns,
                port: 8080,
            };
            machine.core_loop(config).await
        });
    }
    Ok(())
}
