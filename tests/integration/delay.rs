use rattan::core::RattanMachine;
use rattan::env::get_std_env;
use regex::Regex;

#[test]
fn test_delay() {
    let _std_env = get_std_env().unwrap();
    let left_ns = _std_env.left_ns.clone();

    let mut machine = RattanMachine::new();
    let cancel_token = machine.cancel_token();

    let rattan_thread = std::thread::spawn(move || {
        _std_env.rattan_ns.enter().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async move { machine.run(_std_env).await });
    });

    {
        println!("try to ping");
        left_ns.enter().unwrap();
        let output = std::process::Command::new("ping")
            .args(["192.168.2.1", "-c", "10"])
            .output()
            .unwrap();

        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"time=(\d+)").unwrap();
        let mut latency = re.captures_iter(&stdout).map(|cap| cap[1].parse::<u64>()).flatten().collect::<Vec<_>>();

        assert_eq!(latency.len(), 10);
        latency.drain(0..5);
        let average_latency = latency.iter().sum::<u64>() / latency.len() as u64;
        assert!(average_latency > 195 && average_latency < 205);
    }

    cancel_token.cancel();
    rattan_thread.join().unwrap();
}
