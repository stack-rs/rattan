use std::collections::HashMap;
use std::fmt::Display;
use std::fs::File;
use std::io::{Cursor, Result};
use std::path::Path;

use binread::BinRead;
use etherparse::{EtherType, Ethernet2HeaderSlice, IpNumber, Ipv4HeaderSlice, TcpHeaderSlice};
use memmap2::Mmap;
use rattan_log::log_entry::entry::tcp_ip_compact::TCPProtocolEntry;
use rattan_log::log_entry::{LogEntry, PktAction};
use rattan_log::mmap_file;
use rattan_log::TCPLogEntry;
use rattan_log::{ParseContext, TCPTuple};

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum Position {
    Client,
    Server,
}

impl Position {
    fn other_side(&self) -> Self {
        match self {
            Self::Client => Self::Server,
            Self::Server => Self::Client,
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum Direction {
    Recv,
    Sent,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct PacketMatching {
    flow_id: u32,
    seq: u32,
    ack: u32,
    ip_id: u16,
    checksum: u16,
    flags: u8,
    dataofs: u8,
}

#[derive(Debug, Clone, Default)]
pub struct TimePoint {
    pub sender: Option<Position>,
    pub sender_pcap: Option<u64>,
    pub rattan_ingress: Option<u64>,
    pub rattan_logical_leave: Option<u64>,
    pub rattan_egress: Option<u64>,
    pub receiver_pcap: Option<u64>,
}

impl TimePoint {
    fn is_complete(&self) -> bool {
        self.sender_pcap.is_some()
            && self.rattan_ingress.is_some()
            && self.rattan_logical_leave.is_some()
            && self.rattan_egress.is_some()
            && self.receiver_pcap.is_some()
            && self.sender.is_some()
    }

    fn set_sender(&mut self, sender: Position) {
        if let Some(old_value) = self.sender {
            assert_eq!(old_value, sender);
        } else {
            self.sender = Some(sender);
        }
    }
}

fn load_file(path: impl AsRef<Path> + std::fmt::Debug) -> Result<Mmap> {
    eprintln!("Loading file {:?}", path);
    let file = File::open(path)?;
    mmap_file(&file)
}

#[derive(Default)]
struct PacketMatcher {
    seen_in_rtl: HashMap<PacketMatching, TimePoint>,
}

impl PacketMatcher {
    fn add_rtl_packet(&mut self, key: PacketMatching, action: impl FnOnce(&mut TimePoint)) {
        let entry = self.seen_in_rtl.entry(key).or_default();
        action(entry);
    }

    fn debug_stats(&self) {
        eprintln!("    Packets seen in rtl: {}", self.seen_in_rtl.len());
        eprintln!(
            "    Complete items: {}",
            self.seen_in_rtl
                .values()
                .map(|v| v.is_complete())
                .filter(|&b| b)
                .count()
        );
    }

    fn sort_by_rattan_ingress(self) -> Vec<(PacketMatching, TimePoint)> {
        let mut items: Vec<(PacketMatching, TimePoint)> = self.seen_in_rtl.into_iter().collect();
        items.sort_by_key(|(_k, v)| v.rattan_ingress);
        items
    }
}

fn load_pcap(
    path: &Path,
    flows: &HashMap<TCPTuple, u32>,
    packet_matcher: &mut PacketMatcher,
    position: Position,
) -> Result<()> {
    let file = File::open(path)?;
    let mut pcap_reader = pcap_file::pcap::PcapReader::new(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    while let Some(pkt) = pcap_reader.next_packet() {
        let Ok(pkt) = pkt else {
            continue;
        };
        let ts = pkt.timestamp.as_micros() as u64;

        let data = pkt.data;

        let sent_by_us = data[1] == 0x04;

        // dbg!(ts, sent_by_us);

        // Standard Ethernet header is 14 bytes
        // But we get "Linux cooked capture", which is 16 bytes
        let data = &data[2..];

        let Ok(ether_hdr) = Ethernet2HeaderSlice::from_slice(data) else {
            continue;
        };

        if !matches!(ether_hdr.ether_type(), EtherType::IPV4) {
            continue;
        }

        let l2_header_len = ether_hdr.slice().len();
        let l3_packet = data.get(l2_header_len..).unwrap_or(&[]);

        let Ok(ip_hdr) = Ipv4HeaderSlice::from_slice(l3_packet) else {
            continue;
        };

        let src_ip = ip_hdr.source_addr();
        let dst_ip = ip_hdr.destination_addr();

        if !matches!(ip_hdr.protocol(), IpNumber::TCP) {
            continue;
        }

        let ip_id = ip_hdr.identification();
        let ip_checksum = ip_hdr.header_checksum();

        let l3_header_len = ip_hdr.slice().len();
        let l4_packet = l3_packet.get(l3_header_len..).unwrap_or(&[]);

        let Ok(tcp_hdr) = TcpHeaderSlice::from_slice(l4_packet) else {
            continue;
        };

        let src_port = tcp_hdr.source_port();
        let dst_port = tcp_hdr.destination_port();

        let tcp_tuple = TCPTuple {
            src_ip,
            dst_ip,
            src_port,
            dst_port,
        };

        let Some(flow_id) = flows.get(&tcp_tuple).copied() else {
            continue;
        };

        let packet_matching = PacketMatching {
            flow_id,
            seq: tcp_hdr.sequence_number(),
            ack: tcp_hdr.acknowledgment_number(),
            ip_id,
            checksum: ip_checksum,
            flags: tcp_hdr.slice()[13],
            dataofs: tcp_hdr.data_offset(),
        };

        // dbg!(&packet_matching);

        packet_matcher.add_rtl_packet(packet_matching, |entry| {
            if sent_by_us {
                let _ = entry.sender_pcap.insert(ts);
                let _ = entry.sender.insert(position);
            } else {
                let _ = entry.receiver_pcap.insert(ts);
                let _ = entry.sender.insert(position.other_side());
            }
        });
    }

    Ok(())
}

fn load_rtl(path: &Path, matcher: &mut PacketMatcher, base_ts: u64) -> Result<()> {
    // Read rattan_log
    let mut rattan_log_file = Cursor::new(load_file(path)?);
    while let Ok(entry) = LogEntry::read(&mut rattan_log_file) {
        if let LogEntry::CompactTCP(TCPLogEntry {
            general_pkt_entry,
            tcp_entry,
            ..
        }) = entry
        {
            let TCPProtocolEntry {
                flow_id,
                seq,
                ack,
                ip_frag,
                ip_id,
                checksum,
                flags,
                dataofs,
                ..
            } = tcp_entry;

            let ts = general_pkt_entry.ts as u64;
            let action = general_pkt_entry.header.get_pkt_action();
            let action = PktAction::try_from(action).expect("invalid pkt action");

            let from_server = flow_id >= 0x02000000;

            let (position, direction) = match (from_server, action) {
                // sent by endpoints
                (true, PktAction::Recv) => (Position::Server, Direction::Sent),
                (false, PktAction::Recv) => (Position::Client, Direction::Sent),
                // received by endpoints
                (true, PktAction::Send) => (Position::Client, Direction::Recv),
                (false, PktAction::Send) => (Position::Server, Direction::Recv),
                _ => unreachable!(),
            };

            let drift = ip_frag as u64;

            let rtl_time = base_ts + ts;
            let event_time = rtl_time - drift;

            // if matches!(action, PktAction::Send){
            //     println!(
            //         "RTL: {:?} {:?} at {:09} , should be   {:09}",
            //         position,
            //         direction,
            //         rtl_time % 1_000_000_000,
            //         event_time % 1_000_000_000
            //     );
            // }

            // if matches!(action, PktAction::Recv){
            //     println!(
            //         "RTL: {:?} {:?} at {:09} , recorded at {:09}",
            //         position,
            //         direction,
            //         event_time % 1_000_000_000,
            //         rtl_time % 1_000_000_000
            //     );
            // }

            let packet_matching = PacketMatching {
                flow_id,
                seq,
                ack,
                ip_id,
                checksum,
                flags,
                dataofs,
            };

            match action {
                PktAction::Send => {
                    matcher.add_rtl_packet(packet_matching, |entry| {
                        let _ = entry.rattan_logical_leave.insert(event_time);
                        let _ = entry.rattan_egress.insert(rtl_time);
                        if direction == Direction::Sent {
                            entry.set_sender(position);
                        } else {
                            entry.set_sender(position.other_side());
                        }
                    });
                }
                PktAction::Recv => {
                    matcher.add_rtl_packet(packet_matching, |entry| {
                        let _ = entry.rattan_ingress.insert(event_time);
                        if direction == Direction::Sent {
                            entry.set_sender(position);
                        } else {
                            entry.set_sender(position.other_side());
                        }
                    });
                }
                _ => {}
            };
        }
    }
    Ok(())
}

struct TimeDifference {
    diff: Option<i64>,
}

impl From<Option<i64>> for TimeDifference {
    fn from(diff: Option<i64>) -> Self {
        Self { diff }
    }
}

impl Display for TimeDifference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.diff.is_none() {
            return write!(f, "*****");
        }
        write!(f, "{:5}", self.diff.unwrap())
    }
}

fn main() -> Result<()> {
    let artifacts_dir = std::env::var("ARTIFACTS_DIR").unwrap();
    let artifacts_dir = Path::new(&artifacts_dir);
    let client_pcap = artifacts_dir.join("./exec-log/client.pcap");
    let server_pcap = artifacts_dir.join("./exec-log/server.pcap");
    let rattan_flow = artifacts_dir.join("./result/packet.flow");
    let rattan_log = artifacts_dir.join("./result/packet.rtl");

    // Read rattan flow
    let mut flow_entry = vec![];
    let mut rattan_flow_file = Cursor::new(load_file(rattan_flow)?);

    while let Ok(entry) = LogEntry::read(&mut rattan_flow_file) {
        match entry {
            LogEntry::TCPFlow(flow) => flow_entry.push(flow.into()),
            LogEntry::TraceStart(trace_start) => flow_entry.push(trace_start.into()),
            _ => (),
        }
    }

    let context = ParseContext::new(flow_entry);

    for (tcp_tuple, flow_id) in context.tuple_to_flow.iter() {
        eprintln!("    Found flow[0x{:08x}] {}", flow_id, tcp_tuple);
    }

    match context.trace_start_ts {
        Some(trace_start) => eprintln!("Trace start at {}", trace_start),
        None => eprintln!("    Trace start point not found!"),
    }

    let mut matcher = PacketMatcher::default();

    load_rtl(rattan_log.as_path(), &mut matcher, context.base_ts)?;
    load_pcap(
        client_pcap.as_path(),
        &context.tuple_to_flow,
        &mut matcher,
        Position::Client,
    )?;
    load_pcap(
        server_pcap.as_path(),
        &context.tuple_to_flow,
        &mut matcher,
        Position::Server,
    )?;

    matcher.debug_stats();

    let sorted = matcher.sort_by_rattan_ingress();

    fn diff_u64(a: Option<u64>, b: Option<u64>) -> TimeDifference {
        let (Some(a), Some(b)) = (a, b) else {
            return None.into();
        };
        let diff: Option<_> = if a >= b {
            (a - b) as i64
        } else {
            eprintln!("Warning!, {:?} < {:?}", a, b);
            -((b - a) as i64)
        }
        .into();
        diff.into()
    }

    for (_key, value) in sorted {
        // println!("{:?} {:?}", _key, value);
        if value.sender.unwrap() == Position::Client {
            print!("C->S");
        } else {
            print!("S->C");
        }
        let rattan_ingress = diff_u64(value.rattan_ingress, Some(0));
        let before_rattan = diff_u64(value.rattan_ingress, value.sender_pcap);
        let logical_in_rattan = diff_u64(value.rattan_logical_leave, value.rattan_ingress);
        let drift_in_rattan = diff_u64(value.rattan_egress, value.rattan_logical_leave);
        let after_rattan = diff_u64(value.receiver_pcap, value.rattan_egress);

        println!(
            "\t{}\t{}\t{}\t{}\t{}",
            rattan_ingress, before_rattan, logical_in_rattan, drift_in_rattan, after_rattan
        );
    }

    eprintln!();

    Ok(())
}
