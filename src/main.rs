use rattan::{env::get_std_env, core::RattanMachine};

fn main() -> anyhow::Result<()> {
    let _std_env = get_std_env().unwrap();
    let mut machine = RattanMachine::new();

    {
        _std_env.rattan_ns.enter().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async move { machine.run(_std_env).await });
    }
    Ok(())
}
