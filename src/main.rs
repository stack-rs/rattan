use rattan::env::get_std_env;

fn main() -> anyhow::Result<()> {
    let std_env = get_std_env().unwrap();
    std_env.clean().unwrap();
    Ok(())
}
