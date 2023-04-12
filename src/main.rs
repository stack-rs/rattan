use rattan::env::get_std_env;

fn main() -> anyhow::Result<()> {
    let _std_env = get_std_env().unwrap();
    Ok(())
}
