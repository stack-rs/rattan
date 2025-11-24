use std::{
    collections::HashMap,
    fs::File,
    io::{Cursor, Error, ErrorKind, Result},
    net::IpAddr,
    path::{Path, PathBuf},
};

use binread::BinRead;
use memmap2::Mmap;

use crate::{
    log_entry::{
        entry::{
            chunk_header::{ChunkEntry, TYPE_LOG_ENTRY, TYPE_LOG_META},
            flow_entry::FlowEntryVariant,
        },
        general_packet::GeneralPacketType,
        LogEntry, PktAction,
    },
    logger::{
        build_pcap::{add_eth, add_ip, get_mock_mac, PacketWriter},
        mmap::{mmap_file, mmap_segment},
        LOG_ENTRY_CHUNK_SIZE, META_DATA_CHUNK_SIZE,
    },
};

pub fn read_chunk_header<T: AsRef<[u8]>>(
    cursor: &mut Cursor<T>,
    expected_type: u8,
) -> Result<ChunkEntry> {
    let chunk = if let LogEntry::Chunk(chunk_header) =
        LogEntry::read(cursor).map_err(|e| Error::new(ErrorKind::InvalidData, e))?
    {
        chunk_header.chunk
    } else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Expecting a chunk_headr",
        ));
    };

    let log_version = chunk.log_version;
    if log_version != 0x20251120 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Not supported log version {}", log_version),
        ));
    }

    if chunk.header.get_type() != expected_type {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "unexpected log entry type",
        ));
    }

    Ok(chunk)
}

pub fn read_metadata_chunk(
    file: &File,
    output: &mut Vec<FlowEntryVariant>,
    mut next_chunk_offset: u64,
) -> Result<(u64, usize)> {
    let meta_data_chunk = mmap_segment(file, next_chunk_offset, META_DATA_CHUNK_SIZE as usize)?;
    let mut chunk_buf = Cursor::new(meta_data_chunk.as_ref());

    let chunk = read_chunk_header(&mut chunk_buf, TYPE_LOG_META)?;

    // Jump to next page. 0 means to exist.
    next_chunk_offset = chunk.offset;

    let mut entry_count = 0;

    while let Ok(log_entry) = LogEntry::read(&mut chunk_buf) {
        if let LogEntry::TCPFlow(tcp_flow) = log_entry {
            entry_count += 1;
            output.push(tcp_flow.tcp_flow.into());
        }
    }

    Ok((next_chunk_offset, entry_count))
}

pub fn read_log_entry_chunk(
    file: &File,
    output: &mut Vec<LogEntry>,
    mut start_offset: u64,
) -> Result<(u64, usize, u64)> {
    let log_entry_chunk = mmap_segment(file, start_offset, LOG_ENTRY_CHUNK_SIZE)?;
    let mut chunk_buf = Cursor::new(log_entry_chunk.as_ref());

    let chunk = read_chunk_header(&mut chunk_buf, TYPE_LOG_ENTRY)?;

    start_offset += chunk.chunk_size as u64;
    let mut entry_count = 0;

    while let Ok(log_entry) = LogEntry::read(&mut chunk_buf) {
        match log_entry {
            LogEntry::CompactTCP(_) | LogEntry::Raw(_) => {
                output.push(log_entry);
                entry_count += 1;
            }
            _ => {}
        }
    }
    Ok((start_offset, entry_count, chunk.offset))
}

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

        for (index, entry) in input.into_iter().enumerate() {
            let Ok(index) = u16::try_from(index + 1) else {
                tracing::error!("No more than 65535 flows are supported in current version");
                break;
            };
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
    output_file: impl AsRef<Path>,
) -> Result<()> {
    let get_output_path = |ip: &IpAddr| -> PathBuf {
        let mut output_file = output_file.as_ref().to_path_buf();
        let file_name = match output_file.file_name().map(|n| n.to_string_lossy()) {
            Some(prefix) => format!("{}_{:?}.pcapng", prefix, ip),
            _ => format!("{:?}.pcapng", ip),
        };
        output_file.set_file_name(file_name);
        output_file
    };

    let file = File::open(&file_path)?;

    let mut raw_file_path = file_path.as_ref().to_path_buf();
    raw_file_path.set_extension("raw");

    // Unused if the log is not in raw format.
    let mut raw_file: Option<Mmap> = None;
    fn load_raw_file(path: &PathBuf) -> Result<Mmap> {
        mmap_file(&File::open(path)?)
    }

    let mut flow_entry = vec![];

    let mut meta_chunk_offset = 0;

    let mut end_of_log_entry = None;

    loop {
        let (next, _added) = read_metadata_chunk(&file, &mut flow_entry, meta_chunk_offset)?;
        meta_chunk_offset = next;
        if meta_chunk_offset == 0 {
            break;
        }
        end_of_log_entry.get_or_insert(meta_chunk_offset);
    }

    let context = ParseContext::new(flow_entry);

    let start_of_log_entry = META_DATA_CHUNK_SIZE;
    let end_of_log_entry = end_of_log_entry.unwrap_or(file.metadata()?.len());

    let mut writers: HashMap<IpAddr, PacketWriter> = HashMap::new();

    for (_, variant) in context.flows.iter() {
        for ip in variant.get_ip() {
            writers
                .entry(ip)
                .or_insert_with_key(|ip| PacketWriter::new(get_output_path(ip), ip));
        }
    }

    let mut log_entry_offset = start_of_log_entry;
    while log_entry_offset < end_of_log_entry {
        let mut log_entries = vec![];

        let (next_offset, _count, chunk_offset) =
            read_log_entry_chunk(&file, &mut log_entries, log_entry_offset)?;
        log_entry_offset = next_offset;

        for log_entry in std::mem::take(&mut log_entries) {
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
                    if let (Some(writer), FlowEntryVariant::TCP(flow)) =
                        (writers.get_mut(&ip), flow)
                    {
                        writer.write_tcp_packet(flow, compact_tcp)?;
                    }
                }
                LogEntry::Raw(raw_entry) => {
                    let Some(flow_id) = context.flow_index.get(&raw_entry.raw_entry.flow_index)
                    else {
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
                    let offset = (pointer.get_offset() as u64 + chunk_offset) as usize;

                    let raw = match raw_file {
                        Some(ref raw) => raw,
                        None => {
                            // Would enter here only once, upon the very first raw_entry was being processed.
                            let loaded = load_raw_file(&raw_file_path)?;
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
                            0,
                        );
                    }

                    packet_header.extend_from_slice(raw_header);
                    writer.write_raw_packet(&raw_entry, packet_header, context.base_ts)?
                }
                _ => {}
            }
        }
    }

    for writer in writers.into_values() {
        writer.finalize()?;
    }

    Ok(())
}
