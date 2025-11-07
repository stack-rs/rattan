use binread::BinReaderExt;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rattan_core::radix::log::*;

use clap::Args;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Result, Write};
use std::net::IpAddr;

use pcap_file::{
    pcapng::{
        blocks::{
            enhanced_packet::EnhancedPacketBlock,
            interface_description::{InterfaceDescriptionBlock, InterfaceDescriptionOption},
            Block::{EnhancedPacket, InterfaceDescription},
        },
        PcapNgWriter,
    },
    DataLink,
};

#[derive(Debug, Deserialize)]
struct FlowDesc {
    #[serde(rename = "TCP")]
    tcp: (IpAddr, IpAddr, u16, u16),
}

#[derive(Debug, Deserialize)]
struct FlowRecord {
    flow_id: u32,
    base_ts: u64,
    flow_desc: FlowDesc,
}
fn parse_flows_file(path: &Path) -> std::io::Result<Vec<FlowRecord>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let mut flows = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: FlowRecord = serde_json::from_str(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        flows.push(record);
    }
    Ok(flows)
}

fn build_eth_ipv4_tcp_packet(
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    flow_entry: &FlowDesc,
    log_entry: TCPLogEntry,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(54);

    let payload_len = log_entry.general_pkt_entry.pkt_length.saturating_sub(54);

    // --- Ethernet header (14 bytes)
    buf.extend_from_slice(&dst_mac);
    buf.extend_from_slice(&src_mac);
    buf.extend_from_slice(&0x0800u16.to_be_bytes()); // Ethertype = IPv4

    assert_eq!(buf.len(), 14);

    // --- IPv4 header (no options, IHL=5, 20 bytes)

    let (src_ip, dst_ip, src_port, dst_port) = match flow_entry.tcp {
        (IpAddr::V4(src_ip), IpAddr::V4(dst_ip), src_port, dst_port) => {
            (src_ip, dst_ip, src_port, dst_port)
        }
        _ => unimplemented!("IPv4 only."),
    };

    let ihl_version = 0x45u8; // version=4, ihl=5 (20 bytes)
    let dscp_ecn = 0u8;
    let total_len = 20 + 20 + payload_len; // ip header + tcp header + payload
    let identification = log_entry.tcp_entry.ip_id;
    let flags_frag = log_entry.tcp_entry.ip_frag;
    let ttl = 64u8;
    let protocol = 6u8; // TCP

    buf.push(ihl_version);
    buf.push(dscp_ecn);
    buf.extend_from_slice(&total_len.to_be_bytes());
    buf.extend_from_slice(&identification.to_be_bytes());
    buf.extend_from_slice(&flags_frag.to_be_bytes());
    buf.push(ttl);
    buf.push(protocol);
    buf.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    buf.extend_from_slice(&src_ip.octets());
    buf.extend_from_slice(&dst_ip.octets());

    assert_eq!(buf.len(), 34);

    // --- TCP header (no options, 20 bytes)
    let tcp_seq = log_entry.tcp_entry.seq;
    let tcp_ack = log_entry.tcp_entry.ack;
    let window = 65535u16;
    let dataoff_flags =
        log_entry.tcp_entry.flags as u16 + ((log_entry.tcp_entry.dataofs as u16) << 12);

    buf.extend_from_slice(&src_port.to_be_bytes());
    buf.extend_from_slice(&dst_port.to_be_bytes());
    buf.extend_from_slice(&tcp_seq.to_be_bytes());
    buf.extend_from_slice(&tcp_ack.to_be_bytes());
    buf.extend_from_slice(&dataoff_flags.to_be_bytes()); // data offset/reserved + flags will be set below
    buf.extend_from_slice(&window.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    buf.extend_from_slice(&0u16.to_be_bytes()); // urgent pointer

    assert_eq!(buf.len(), 54);
    buf
}

struct PacketWriter {
    path: PathBuf,
    writer: PcapNgWriter<Vec<u8>>,
}

fn get_mock_mac(addr: IpAddr) -> [u8; 6] {
    match addr {
        IpAddr::V4(ip) => {
            let ip = ip.octets();
            [0x38, 0x7e, 0x58, ip[1], ip[2], ip[3]]
        }
        IpAddr::V6(_) => todo!(),
    }
}
impl PacketWriter {
    fn new(path: PathBuf, ip: &IpAddr) -> Self {
        let mut writer = PcapNgWriter::new(vec![]).unwrap();
        let interface = InterfaceDescriptionBlock {
            linktype: DataLink::ETHERNET,
            snaplen: 0xFFFF,
            // XXX: Solve the problem <https://github.com/courvoif/pcap-file/pull/32>
            options: vec![
                InterfaceDescriptionOption::IfName(std::borrow::Cow::Owned(format!("{:?}", ip))),
                InterfaceDescriptionOption::IfDescription(std::borrow::Cow::Owned(format!(
                    "Interface for IP: {:?}",
                    ip
                ))),
                InterfaceDescriptionOption::IfTsResol(9),
            ],
        };
        writer
            .write_block(&InterfaceDescription(interface))
            .unwrap();

        Self { path, writer }
    }

    fn write_tcp_packet(&mut self, flow: &FlowRecord, tcp_entry: TCPLogEntry) -> Result<()> {
        let data = build_eth_ipv4_tcp_packet(
            get_mock_mac(flow.flow_desc.tcp.0),
            get_mock_mac(flow.flow_desc.tcp.1),
            &flow.flow_desc,
            tcp_entry,
        );

        let enhanced_packet = EnhancedPacketBlock {
            interface_id: 0,
            timestamp: Duration::from_micros(
                tcp_entry.general_pkt_entry.ts as u64 + flow.base_ts / 1000_u64,
            ),
            original_len: tcp_entry.general_pkt_entry.pkt_length as u32,
            data: Cow::from(data),
            options: vec![],
        };

        let _ = self
            .writer
            .write_block(&EnhancedPacket(enhanced_packet))
            .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn finalize(mut self) -> Result<()> {
        eprintln!("Writing into {}", self.path.display());
        let mut file_out = File::create(&self.path)?;
        file_out.write_all(self.writer.get_mut())
    }
}

/// Convert Rattan Packet Log file to pcapng file for each end
#[derive(Args, Debug, Default, Clone)]
#[command(rename_all = "kebab-case")]
pub struct LogConverterArgs {
    /// Input Rattan Packet Log file path
    pub input: PathBuf,
    /// Output pcapng file name prefix
    pub output: PathBuf,
}

pub fn convert_log_to_pcapng(input: impl AsRef<Path>, output: impl AsRef<Path>) -> Result<()> {
    let input_log_file: &Path = input.as_ref();
    let input_flow_file_name = input_log_file
        .file_name()
        .map(|n| format!("{}.flows", n.to_str().unwrap()))
        .expect("input is not a file");
    let mut input_flow_file = input_log_file.to_path_buf();
    input_flow_file.set_file_name(input_flow_file_name);

    let output_file: &Path = output.as_ref();

    let get_output_path = |ip: &IpAddr| -> PathBuf {
        let mut output_file = output_file.to_path_buf();
        let file_name = match output_file.file_name().map(|n| n.to_string_lossy()) {
            Some(prefix) => format!("{}_{:?}.pcapng", prefix, ip),
            _ => format!("{:?}.pcapng", ip),
        };
        output_file.set_file_name(file_name);
        output_file
    };

    eprintln!(
        "Converting {:?} {:?}-> {:?}",
        input_log_file.display(),
        input_flow_file.display(),
        output_file.display()
    );

    let flows = parse_flows_file(&input_flow_file)?;

    let mut writers: HashMap<IpAddr, PacketWriter> = HashMap::new();
    let mut flows_map: HashMap<u32, FlowRecord> = HashMap::new();

    for line in flows {
        let src_ip: IpAddr = line.flow_desc.tcp.0;
        let dst_ip: IpAddr = line.flow_desc.tcp.1;
        for ref ip in [src_ip, dst_ip] {
            if writers.contains_key(ip) {
                continue;
            }
            writers.insert(*ip, PacketWriter::new(get_output_path(ip), ip));
        }
        flows_map.insert(line.flow_id, line);
    }

    let mut logs = File::open(input_log_file)?;

    while let Ok(entry) = logs.read_le::<ConvertedLogEntry>() {
        if let ConvertedLogEntry::TCP(tcp_entry) = entry {
            let flow_id = tcp_entry.tcp_entry.flow_id;
            if let Some(flow) = flows_map.get(&flow_id) {
                let ip =
                    match PktAction::try_from(tcp_entry.general_pkt_entry.header.get_pkt_action())
                        .ok()
                    {
                        Some(PktAction::Recv) => flow.flow_desc.tcp.0,
                        Some(PktAction::Send) => flow.flow_desc.tcp.1,
                        _ => continue,
                    };
                if let Some(writer) = writers.get_mut(&ip) {
                    writer.write_tcp_packet(flow, tcp_entry)?;
                }
            }
        }
    }

    for (_, writer) in writers {
        writer.finalize()?;
    }

    Ok(())
}
