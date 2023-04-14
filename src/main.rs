use rattan::core::core_loop;
use rattan::env::get_std_env;

fn main() -> anyhow::Result<()> {
    let _std_env = get_std_env().unwrap();
    {
        _std_env.rattan_ns.enter().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async move { core_loop(_std_env).await });
    }
    Ok(())
}
