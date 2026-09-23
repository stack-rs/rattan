use std::fs::{File, remove_file};
use std::io::{Read, Result, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use clap::{Arg, ArgAction, Command};

const SEND_CHUNK: usize = 1500 * 64;

fn run_server(addr: SocketAddr, lock: &Path) -> Result<()> {
    let listener = TcpListener::bind(addr)?;
    File::create(lock)?;

    let (mut stream, peer) = listener.accept()?;
    println!("accepted {peer}");

    let begin = Instant::now();
    let mut total = 0usize;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n;
    }

    println!(
        "=========RECEIVE_RESULT========= Received {} bytes from client in {} s.",
        total,
        begin.elapsed().as_secs_f64()
    );
    Ok(())
}

fn run_client(addr: SocketAddr, bytes: usize) -> Result<()> {
    let buffer = vec![0u8; SEND_CHUNK];

    let begin = Instant::now();
    let mut stream = TcpStream::connect(addr)?;

    let mut remaining = bytes;
    while remaining > 0 {
        let n = remaining.min(buffer.len());
        stream.write_all(&buffer[..n])?;
        remaining -= n;
    }
    drop(stream);

    println!(
        "=========RCT_RESULT========= Sent {} bytes to server in {} s.",
        bytes,
        begin.elapsed().as_secs_f64()
    );
    Ok(())
}

fn wait_for(lock: &Path) -> Result<()> {
    while !lock.try_exists()? {
        sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn main() -> Result<()> {
    let matches = Command::new("mptcp_app")
        .about("Time a fixed-size transfer over one TCP connection.")
        .arg(
            Arg::new("server")
                .short('s')
                .long("server")
                .help("Receive and count")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("client")
                .short('c')
                .long("client")
                .help("Connect and send")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("address")
                .short('a')
                .long("address")
                .value_parser(clap::value_parser!(Ipv4Addr))
                .help("Address to connect to, or to listen on")
                .default_value("127.0.0.1"),
        )
        .arg(
            Arg::new("port")
                .short('p')
                .long("port")
                .value_parser(clap::value_parser!(u16))
                .help("Port to connect to, or to listen on")
                .default_value("5201"),
        )
        .arg(
            Arg::new("bytes")
                .short('b')
                .long("bytes")
                .value_parser(clap::value_parser!(usize))
                .help("How many bytes the client sends")
                .default_value("53433800"),
        )
        .arg(
            Arg::new("file")
                .short('f')
                .long("file")
                .value_parser(clap::value_parser!(String))
                .help("Lock file: the server creates it, the client waits for it")
                .required(true),
        )
        .get_matches();

    let addr = SocketAddr::new(
        (*matches.get_one::<Ipv4Addr>("address").unwrap()).into(),
        *matches.get_one::<u16>("port").unwrap(),
    );
    let lock = Path::new(matches.get_one::<String>("file").unwrap());

    if matches.get_flag("server") {
        run_server(addr, lock)
    } else if matches.get_flag("client") {
        wait_for(lock)?;
        let result = run_client(addr, *matches.get_one::<usize>("bytes").unwrap());
        remove_file(lock)?;
        result
    } else {
        eprintln!("give either --server or --client");
        std::process::exit(2);
    }
}
