use crate::{log_entry::entry::flow_entry::TCPFlow, RawLogEntry, TCPLogEntry};

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

use std::borrow::Cow;

use std::fs::File;
use std::io::{Result, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

pub fn add_eth(buf: &mut Vec<u8>, dst_mac: [u8; 6], src_mac: [u8; 6]) {
    buf.extend_from_slice(&dst_mac);
    buf.extend_from_slice(&src_mac);
    buf.extend_from_slice(&0x0800u16.to_be_bytes()); // Ethertype = IPv4
}

pub fn add_ip(buf: &mut Vec<u8>, src_ip: u32, dst_ip: u32, pkt_len: u16, ip_id: u16, ip_frag: u16) {
    let ihl_version = 0x45u8; // version=4, ihl=5 (20 bytes)
    let dscp_ecn = 0u8;
    let total_len = pkt_len.saturating_sub(14); // excluding 14B L2 header
    let identification = ip_id;
    let flags_frag = ip_frag;
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
    buf.extend_from_slice(&src_ip.to_be_bytes());
    buf.extend_from_slice(&dst_ip.to_be_bytes());
}

fn build_eth_ipv4_tcp_packet(
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    flow_entry: &TCPFlow,
    log_entry: TCPLogEntry,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(54);

    let pkt_len = log_entry.general_pkt_entry.pkt_length;

    // --- Ethernet header (14 bytes)
    add_eth(&mut buf, dst_mac, src_mac);
    assert_eq!(buf.len(), 14);

    // --- IPv4 header (no options, IHL=5, 20 bytes)

    let (src_ip, dst_ip, src_port, dst_port) = (
        flow_entry.src_ip,
        flow_entry.dst_ip,
        flow_entry.src_port,
        flow_entry.dst_port,
    );

    add_ip(
        &mut buf,
        src_ip,
        dst_ip,
        pkt_len,
        log_entry.tcp_entry.ip_id,
        log_entry.tcp_entry.ip_frag,
    );

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

pub struct PacketWriter {
    path: PathBuf,
    writer: PcapNgWriter<Vec<u8>>,
}

pub fn get_mock_mac(addr: u32) -> [u8; 6] {
    let ip = addr.to_be_bytes();
    [0x38, 0x7e, 0x58, ip[1], ip[2], ip[3]]
}

impl PacketWriter {
    pub fn new(path: PathBuf, ip: &IpAddr) -> Self {
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

    pub fn write_tcp_packet(&mut self, flow_desc: &TCPFlow, tcp_entry: TCPLogEntry) -> Result<()> {
        let data = build_eth_ipv4_tcp_packet(
            get_mock_mac(flow_desc.src_ip),
            get_mock_mac(flow_desc.dst_ip),
            flow_desc,
            tcp_entry,
        );

        let enhanced_packet = EnhancedPacketBlock {
            interface_id: 0,
            timestamp: Duration::from_micros(
                tcp_entry.general_pkt_entry.ts as u64 + flow_desc.base_ts as u64 / 1000,
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

    pub fn write_raw_packet(
        &mut self,
        raw_log_entry: &RawLogEntry,
        packet: Vec<u8>,
        base_ts: i64,
    ) -> Result<()> {
        let enhanced_packet = EnhancedPacketBlock {
            interface_id: 0,
            timestamp: Duration::from_micros(
                raw_log_entry.general_pkt_entry.ts as u64 + base_ts as u64 / 1000,
            ),
            original_len: raw_log_entry.general_pkt_entry.pkt_length as u32,
            data: Cow::from(packet),
            options: vec![],
        };

        let _ = self
            .writer
            .write_block(&EnhancedPacket(enhanced_packet))
            .map_err(std::io::Error::other)?;

        Ok(())
    }
    pub fn finalize(mut self) -> Result<()> {
        eprintln!("Writing into {}", self.path.display());
        let mut file_out = File::create(&self.path)?;
        file_out.write_all(self.writer.get_mut())
    }
}
