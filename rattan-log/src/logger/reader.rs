use std::{
    collections::HashMap,
    fs::File,
    io::{Cursor, Result},
    net::IpAddr,
    path::{Path, PathBuf},
};

use binread::BinRead;
use memmap2::Mmap;

use crate::{
    log_entry::{
        entry::flow_entry::FlowEntryVariant, general_packet::GeneralPacketType, LogEntry, PktAction,
    },
    logger::{
        mmap::mmap_file,
        pcap::{add_eth, add_ip, get_mock_mac, PacketWriter},
    },
};

type FlowId = u32;
type FlowIndex = u16;

#[derive(Clone)]
struct ParseContext {
    pub flows: HashMap<FlowId, FlowEntryVariant>,
    pub flow_index: HashMap<FlowIndex, FlowId>,
    pub base_ts: i64,
}

impl ParseContext {
    pub fn new(input: Vec<FlowEntryVariant>) -> Self {
        let mut flows = HashMap::new();
        let mut flow_index = HashMap::new();
        let mut base_ts = None;

        if input.len() > u16::MAX as usize {
            tracing::error!("No more than 65535 flows are supported in current version. It will not be recorded from the 65,536th stream forward.");
        }

        for (index, entry) in input.into_iter().enumerate().take(u16::MAX as usize) {
            // flow_index 0 is reserved. So start form 1.
            let index = (index + 1) as u16;
            match entry {
                FlowEntryVariant::TCP(tcp) => {
                    let flow_id = tcp.flow_id;
                    flows.insert(flow_id, entry);
                    base_ts.get_or_insert(tcp.base_ts);
                    flow_index.insert(index, tcp.flow_id);
                }
            }
        }
        // In current version of Rattan, all the flows share a common `base_ts`;
        let base_ts = base_ts.unwrap_or_default();

        Self {
            flows,
            flow_index,
            base_ts,
        }
    }
}

pub fn get_ip(flow: &FlowEntryVariant, packet_action: u8) -> Option<IpAddr> {
    match PktAction::try_from(packet_action).ok()? {
        PktAction::Recv => flow.get_src_ip().into(),
        PktAction::Send => flow.get_dst_ip().into(),
        _ => None,
    }
}

pub fn convert_log_to_pcapng(
    file_path: impl AsRef<Path>,
    output_file: Option<impl AsRef<Path>>,
) -> Result<()> {
    fn load_file_with_mmap(path: impl AsRef<Path>) -> Result<Mmap> {
        mmap_file(&File::open(path)?)
    }
    let output_file = if let Some(output_file) = output_file {
        output_file.as_ref().to_path_buf()
    } else {
        file_path.as_ref().to_path_buf()
    };

    let get_output_path = |ip: &IpAddr| -> PathBuf {
        let mut output_file = output_file.to_path_buf();
        let file_name = match output_file.file_name().map(|n| n.to_string_lossy()) {
            Some(prefix) => format!("{}_{}.pcapng", prefix, ip.to_string().replace(".", "_")),
            _ => format!("{:?}.pcapng", ip),
        };
        output_file.set_file_name(file_name);
        output_file
    };

    let log_entry_file: Mmap = load_file_with_mmap(file_path.as_ref())?;

    let mut flow_file_path = file_path.as_ref().to_path_buf();
    flow_file_path.set_extension("flow");
    let flow_file: Mmap = load_file_with_mmap(&flow_file_path)?;

    let mut raw_file_path = file_path.as_ref().to_path_buf();
    raw_file_path.set_extension("raw");

    // Unused if the log is not in raw format.
    let mut raw_file: Option<Mmap> = None;

    let mut flow_entry = vec![];

    let mut flows = Cursor::new(flow_file);
    while let Ok(entry) = LogEntry::read(&mut flows) {
        if let LogEntry::TCPFlow(flow) = entry {
            flow_entry.push(FlowEntryVariant::TCP(flow.tcp_flow));
        }
    }

    let context = ParseContext::new(flow_entry);

    let mut writers: HashMap<IpAddr, PacketWriter> = HashMap::new();

    for (_, variant) in context.flows.iter() {
        for ip in variant.get_ip() {
            writers
                .entry(ip)
                .or_insert_with_key(|ip| PacketWriter::new(get_output_path(ip), ip));
        }
    }

    let mut chunk_offset = None;
    let mut log_entries = Cursor::new(log_entry_file);

    let mut current_chunk_end: Option<u64> = None;
    let mut next_chunk_start: Option<u64> = None;

    loop {
        let mut current_position = log_entries.position();

        if let Some(currnet_chunk_end) = current_chunk_end {
            // If we are reading beyond the range designated by last chunk header
            if current_position >= currnet_chunk_end {
                // Make sure there is at most 1 jump for each chunk, thus no infinite loop.
                if let Some(next_chunk_start) = next_chunk_start.take() {
                    log_entries.set_position(next_chunk_start);
                    current_position = next_chunk_start;
                } else {
                    break;
                }
            }
        }

        let log_entry = match LogEntry::read(&mut log_entries) {
            Ok(log_entry) => log_entry,
            Err(e) => {
                // Make sure there is at most 1 jump for each chunk, thus no infinite loop.
                if let Some(next_chunk_start) = next_chunk_start.take() {
                    tracing::warn!(
                        "Due to error {:?}, skipped remained part of the chunk, or [{},{})",
                        e,
                        current_position,
                        next_chunk_start
                    );
                    log_entries.set_position(next_chunk_start);
                    continue;
                } else {
                    break;
                }
            }
        };

        match log_entry {
            LogEntry::CompactTCP(compact_tcp) => {
                let flow_id = compact_tcp.tcp_entry.flow_id;
                let action = compact_tcp.general_pkt_entry.header.get_pkt_action();
                let Some(flow) = context.flows.get(&flow_id) else {
                    continue;
                };
                let Some(ip) = get_ip(flow, action) else {
                    continue;
                };
                if let (Some(writer), FlowEntryVariant::TCP(flow)) = (writers.get_mut(&ip), flow) {
                    writer.write_tcp_packet(flow, compact_tcp)?;
                }
            }

            LogEntry::Chunk(chunk) => {
                chunk_offset = chunk.chunk.offset.into();
                current_chunk_end = Some(current_position as u64 + chunk.chunk.data_length as u64);
                next_chunk_start = Some(current_position as u64 + chunk.chunk.chunk_size as u64);
            }
            LogEntry::Raw(raw_entry) => {
                let Some(flow_id) = context.flow_index.get(&raw_entry.raw_entry.flow_index) else {
                    continue;
                };
                let Some(flow) = context.flows.get(flow_id) else {
                    continue;
                };
                let action = raw_entry.general_pkt_entry.header.get_pkt_action();
                let Some(ip) = get_ip(flow, action) else {
                    continue;
                };

                let Some(writer) = writers.get_mut(&ip) else {
                    continue;
                };

                let pointer = raw_entry.raw_entry.pointer;
                let len = pointer.get_length() as usize;
                let offset =
                    (pointer.get_offset() as u64 + chunk_offset.unwrap_or_default()) as usize;

                let raw = match raw_file {
                    Some(ref raw) => raw,
                    None => {
                        // Would enter here only once, upon the very first raw_entry was being processed.
                        let loaded = load_file_with_mmap(&raw_file_path)?;
                        raw_file.insert(loaded)
                    }
                };

                let Some(raw_header) = raw.get(offset..(offset + len)) else {
                    continue;
                };

                let (mut packet_header, mock_l3) =
                    match raw_entry.general_pkt_entry.header.get_type().try_into() {
                        Ok(GeneralPacketType::RawIP) => {
                            (Vec::with_capacity(14 + raw_header.len()), false)
                        }
                        Ok(GeneralPacketType::RawTCPIP) => {
                            (Vec::with_capacity(34 + raw_header.len()), true)
                        }
                        _ => {
                            continue;
                        }
                    };

                let (IpAddr::V4(src_ip), IpAddr::V4(dst_ip)) =
                    (flow.get_src_ip(), flow.get_dst_ip())
                else {
                    continue;
                };

                add_eth(
                    &mut packet_header,
                    get_mock_mac(dst_ip.to_bits()),
                    get_mock_mac(src_ip.to_bits()),
                );

                if mock_l3 {
                    add_ip(
                        &mut packet_header,
                        src_ip.to_bits(),
                        dst_ip.to_bits(),
                        raw_entry.general_pkt_entry.pkt_length,
                        0,
                        0x4000, // Don't Fragment
                    );
                }

                packet_header.extend_from_slice(raw_header);
                writer.write_raw_packet(&raw_entry, packet_header, context.base_ts)?
            }

            // Flows are not expected
            _ => {}
        }
    }

    for writer in writers.into_values() {
        writer.finalize()?;
    }

    Ok(())
}
