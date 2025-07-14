#!/usr/bin/env python3
# Convert Rattan Packet Log file to pcapng file for each end.
# Now it only supports TCP Log Entries.
#
# The detailed spec of a TCP Log Entry:
# 0                   1                   2                   3
# 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |       LH.length       | LH.ty.|   GPH.length  |GPH.ac.|GPH.ty.|
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                          GP.timestamp                         |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |           GP.length           |       PRH.length      |PRH.ty.|
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                          tcp.flow_id                          |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                            tcp.seq                            |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                            tcp.ack                            |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |             ip.id             |            ip.frag            |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |          ip.checksum          |   tcp.flags   |  tcp.dataofs  |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

import argparse
import ctypes
import json
import os
import sys

import pcapng.blocks as blocks
import pcapng.constants.link_types as link_types
from pcapng import FileWriter
from scapy.layers.inet import Ether, IP, TCP

c_uint8 = ctypes.c_uint8
c_uint16 = ctypes.c_uint16
c_uint32 = ctypes.c_uint32


def get_bits(value: int, high: int, low: int) -> int:
    return (value >> low) & ((1 << (high - low + 1)) - 1)


# Packet with different actions will be represented with different interfaces
PktAction = {
    0: "Send",
    1: "Recv",
    2: "Drop",
    3: "Passthrough",
}


class LogEntryHeader(ctypes.LittleEndianStructure):
    _pack_ = 2
    _fields_ = [
        ("length", c_uint16, 12),
        ("type", c_uint16, 4),
    ]

    def __repr__(self):
        return f"LogEntryHeader(length={self.length}, type={self.type})"


class GeneralPktHeader(ctypes.LittleEndianStructure):
    _pack_ = 2
    _fields_ = [
        ("length", c_uint16, 8),
        ("pkt_action", c_uint16, 4),
        ("type", c_uint16, 4),
    ]

    def __repr__(self):
        return f"GeneralPktHeader(length={self.length}, pkt_action={self.pkt_action}, type={self.type})"


class GeneralPktEntry(ctypes.LittleEndianStructure):
    _pack_ = 2
    _fields_ = [
        ("header", GeneralPktHeader),
        ("ts", c_uint32),
        ("pkt_length", c_uint16),
    ]

    def __repr__(self):
        return f"GeneralPktEntry(header={self.header}, ts={self.ts}, pkt_length={self.pkt_length})"


class ProtocolHeader(ctypes.LittleEndianStructure):
    _pack_ = 2
    _fields_ = [
        ("length", c_uint16, 12),
        ("type", c_uint16, 4),
    ]

    def __repr__(self):
        return f"ProtocolHeader(length={self.length}, type={self.type})"


class TCPProtocolEntry(ctypes.LittleEndianStructure):
    _pack_ = 2
    _fields_ = [
        ("header", ProtocolHeader),
        ("flow_id", c_uint32),
        ("seq", c_uint32),
        ("ack", c_uint32),
        ("ip_id", c_uint16),
        ("ip_frag", c_uint16),
        ("checksum", c_uint16),
        ("flags", c_uint8),
        ("dataofs", c_uint8),
    ]

    def __repr__(self):
        return (
            f"TCPProtocolEntry(header={self.header}, flow_id={self.flow_id}, "
            f"seq={self.seq}, ack={self.ack}, ip_id={self.ip_id}, ip_frag={self.ip_frag}, "
            f"checksum={self.checksum}, flags={self.flags}, dataofs={self.dataofs})"
        )


class TCPLogEntry(ctypes.LittleEndianStructure):
    _pack_ = 2
    _fields_ = [
        ("header", LogEntryHeader),
        ("general_pkt", GeneralPktEntry),
        ("tcp", TCPProtocolEntry),
    ]

    def __repr__(self):
        return f"TCPLogEntry(header={self.header}, general_pkt={self.general_pkt}, tcp={self.tcp})"


class FlowEntry:
    def __init__(self, entry: str):
        data = json.loads(entry)
        self.flow_id = int(data["flow_id"])
        self.base_ts = int(data["base_ts"])
        self.flow_desc: dict[str, list[str]] = data["flow_desc"]

    def __repr__(self):
        return f"FlowEntry(flow_id={self.flow_id}, base_ts={self.base_ts}, flow_desc={self.flow_desc})"


class LogEntry:
    @staticmethod
    def from_bytes(buf: bytearray):
        if len(buf) < 2:
            raise ValueError("Buffer too short to contain LogEntryHeader")

        LH = LogEntryHeader.from_buffer(buf)
        total_len = LH.length
        if len(buf) < total_len:
            raise ValueError(
                f"Incomplete buffer: expected {total_len} bytes, got {len(buf)}"
            )

        entry_buf = buf[2:total_len]
        if LH.type == 0:  # General Packet Log
            if len(entry_buf) < 2:
                raise ValueError("Too short for GeneralPktHeader")
            GPH = GeneralPktHeader.from_buffer(entry_buf)

            if GPH.type == 0:  # General Packet
                if len(entry_buf) < GPH.length:
                    raise ValueError("Too short for GeneralPktEntry")
                GP = GeneralPktEntry.from_buffer(entry_buf)
            else:
                print(f"Unsupported GeneralPktHeader type: {GPH.type}")
                return None

            proto_buf = entry_buf[2 + GP.header.length :]
            if len(proto_buf) < 2:
                raise ValueError("Too short for ProtocolHeader")
            PRH = ProtocolHeader.from_buffer(proto_buf)

            if PRH.type == 0:  # TCP Protocol
                if len(proto_buf) < PRH.length:
                    raise ValueError("Too short for TCPProtocolEntry")
                return TCPLogEntry.from_buffer(buf)
            else:
                print(f"Unsupported ProtocolHeader type: {PRH.type}")
                return None

        else:  # Unsupported log entry type
            return None


def iter_log_entries(buf: bytes):
    offset = 0
    total_len = len(buf)

    while offset + 2 <= total_len:
        LH = LogEntryHeader.from_buffer_copy(buf, offset)
        length = LH.length

        if offset + length > total_len:
            raise ValueError(f"Truncated entry at offset {offset}")

        entry_buf = buf[offset : offset + length]
        entry = LogEntry.from_bytes(bytearray(entry_buf))

        yield entry, length
        offset += length


class ConvertResultWriter:
    def __init__(self, ip: str):
        self.ip: str = ip
        self.id = None
        self.f_name: str = ""
        self.f_out = None
        self.writer = None
        self.shb = None
        self.tcp_count: int = 0

    def __del__(self):
        print(f"Wrote {self.tcp_count} TCP packets to {self.f_name}")
        if self.f_out:
            self.f_out.close()
            self.f_out = None

    def __repr__(self):
        return f"ConvertResult(ip={self.ip}, id={self.id}, tcp_count={self.tcp_count})"

    def create_writer(self, output_file: str):
        self.f_name = output_file
        self.f_out = open(output_file, "wb")
        self.shb = blocks.SectionHeader()
        self.writer = FileWriter(self.f_out, self.shb)

    def write_new_member(self, blk_type, **kwargs):
        if self.shb is None or self.writer is None:
            raise RuntimeError("Writer not initialized. Call create_writer first.")
        blk = self.shb.new_member(blk_type, **kwargs)
        self.writer.write_block(blk)
        return blk


def convert_log_to_pcapng(input_file: str, output_base: str):
    if not os.path.exists(input_file):
        print(f"Log file {input_file} does not exist. Exiting.")
        sys.exit(1)

    # Read flow file
    flow_file = input_file + ".flows"
    flow_map: dict[int, FlowEntry] = {}
    ip_writer_map: dict[str, ConvertResultWriter] = {}
    if not os.path.exists(flow_file):
        print(f"Flow file {flow_file} does not exist. Exiting.")
        sys.exit(1)
    with open(flow_file, "r") as f:
        for line in f:
            entry = FlowEntry(line.strip())
            flow_map[entry.flow_id] = entry
            if "TCP" in entry.flow_desc:
                src_ip, dst_ip, src_port, dst_port = entry.flow_desc["TCP"]
                if src_ip not in ip_writer_map:
                    ip_writer_map[src_ip] = ConvertResultWriter(src_ip)
                if dst_ip not in ip_writer_map:
                    ip_writer_map[dst_ip] = ConvertResultWriter(dst_ip)

    # Create pcapng file for each ip
    if output_base.endswith(".pcapng"):
        output_base = output_base[:-7]
    elif output_base.endswith(".pcap"):
        output_base = output_base[:-5]
    for ip in ip_writer_map:
        output_file = f"{output_base}_{ip.replace('.', '_')}.pcapng"
        print(f"Create {output_file} for IP {ip}")

        ip_writer_map[ip].create_writer(output_file)
        blk = ip_writer_map[ip].write_new_member(
            blocks.InterfaceDescription,
            link_type=link_types.LINKTYPE_ETHERNET,
            snaplen=65535,
            options={
                "if_name": ip,
                "if_description": f"Interface for IP: {ip}",
                # if_tsresol left to default value (microseconds)
            },
        )
        ip_writer_map[ip].id = blk.interface_id

    with open(input_file, "rb") as f_in:
        data = f_in.read()
        for entry, used in iter_log_entries(data):
            # print(f"Processing entry: {entry}")
            if isinstance(entry, TCPLogEntry):
                # Use microseconds for ts
                base_ts = flow_map[entry.tcp.flow_id].base_ts // 1000
                pkt_ts = base_ts + entry.general_pkt.ts

                src_ip, dst_ip, src_port, dst_port = flow_map[
                    entry.tcp.flow_id
                ].flow_desc["TCP"]

                # Construct packet using Scapy
                ether = Ether(
                    dst="38:7e:58:{:02x}:{:02x}:{:02x}".format(
                        int(dst_ip.split(".")[1]),
                        int(dst_ip.split(".")[2]),
                        int(dst_ip.split(".")[3]),
                    ),
                    src="38:7e:58:{:02x}:{:02x}:{:02x}".format(
                        int(src_ip.split(".")[1]),
                        int(src_ip.split(".")[2]),
                        int(src_ip.split(".")[3]),
                    ),
                )
                ip = IP(
                    src=src_ip,
                    dst=dst_ip,
                    len=entry.general_pkt.pkt_length
                    - 14,  # 14 bytes for Ethernet header
                    id=entry.tcp.ip_id,
                    flags=entry.tcp.ip_frag >> 13,
                    frag=entry.tcp.ip_frag & 0x1FFF,
                    chksum=entry.tcp.checksum,
                )
                tcp = TCP(
                    sport=src_port,
                    dport=dst_port,
                    dataofs=entry.tcp.dataofs,
                    seq=entry.tcp.seq,
                    ack=entry.tcp.ack,
                    flags=entry.tcp.flags,
                    window=65535,  # Set to maximum for simplicity
                    chksum=0,  # Set to 0 for simplicity
                )
                packet = ether / ip / tcp

                # Write packet
                ip = None
                match entry.general_pkt.header.pkt_action:
                    case 0:  # Send from Rattan
                        ip = dst_ip
                    case 1:  # Recv to Rattan
                        ip = src_ip
                    case 2:  # Drop
                        print(f"Dropping packet for flow {entry.tcp.flow_id}")
                    case 3:  # Passthrough
                        print(f"Passthrough packet for flow {entry.tcp.flow_id}")

                if ip is not None:
                    ip_writer_map[ip].write_new_member(
                        blocks.EnhancedPacket,
                        interface_id=ip_writer_map[ip].id,
                        timestamp_high=int(pkt_ts) >> 32,
                        timestamp_low=int(pkt_ts) & 0xFFFFFFFF,
                        packet_len=entry.general_pkt.pkt_length,
                        packet_data=bytes(packet),
                    )
                    ip_writer_map[ip].tcp_count += 1
            else:
                pass


def main():
    parser = argparse.ArgumentParser(
        description="Convert Rattan Packet Log file to pcapng file for each end"
    )
    parser.add_argument("input", help="Input Rattan Packet Log file path")
    parser.add_argument("output", help="Output pcapng file name prefix")

    args = parser.parse_args()

    convert_log_to_pcapng(args.input, args.output)


if __name__ == "__main__":
    main()
