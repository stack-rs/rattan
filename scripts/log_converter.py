#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "python-pcapng>=2.1.1",
#   "scapy>=2.6.1",
# ]
# ///

# Convert Rattan Packet Log file to pcapng file for each end.
# It can process the following types of Log Entries:
#
# The detailed spec of a TCP Log Entry:  (in compact-tcp mode)
#  0                   1                   2                   3
#  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
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
#
# The detailed spec of a raw TCP Log Entry:
#  0                   1                   2                   3
#  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |       LH.length       | LH.ty.|   GPH.length  |GPH.ac.|GPH.ty.|
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                          GP.timestamp                         |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |           GP.length           |         RGP.flow_index        |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |              RGP.relative_offset              |    RGP.len.   |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
#
# The detailed spec of a TCP Flow Entry:
#  0                   1                   2                   3
#  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |       LH.length       | LH.ty.|       FLE.length      |FLE.ty.|
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                             FLE.id                            |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                             src.ip                            |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                             dst.ip                            |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |            src.port           |            dst.port           |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                                                               |
# +                       FLE.base_timestamp                      +
# |                                                               |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                            Reserved                           |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                                                               |
# +                                                               +
# |                                                               |
# +                                                               +
# |                                                               |
# +                                                               +
# |                                                               |
# +                                                               +
# |                                                               |
# +                      FLE.tcp_syn_options                      +
# |                                                               |
# +                                                               +
# |                                                               |
# +                                                               +
# |                                                               |
# +                                                               +
# |                                                               |
# +                                                               +
# |                                                               |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
#
# The detailed spec of a Chunk Prologue:
#  0                   1                   2                   3
#  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |       LH.length       | LH.ty.|       CH.length       | CH.ty.|
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                        CPH.log_version                        |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                        CPH.data_length                        |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                        CPH.chunk_length                       |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                                                               |
# +                           CP.offset                           +
# |                                                               |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                                                               |
# +                            Reserved                           +
# |                                                               |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

import argparse
import ctypes
from ipaddress import IPv4Address
from pathlib import Path
from scapy.layers.inet import IP, TCP
from scapy.layers.l2 import Ether
from scapy.utils import PcapNgWriter, wrpcapng

c_uint8 = ctypes.c_uint8
c_uint16 = ctypes.c_uint16
c_uint32 = ctypes.c_uint32
c_uint64 = ctypes.c_uint64


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


class FlowEntryHeader(ctypes.LittleEndianStructure):
    _pack_ = 2
    _fields_ = [
        ("length", c_uint16, 12),
        ("type", c_uint16, 4),
    ]

    def __repr__(self):
        return f"FlowEntryHeader(length={self.length}, type={self.type})"


class TCPFlowLogEntry(ctypes.LittleEndianStructure):
    _pack_ = 2
    _fields_ = [
        ("header", FlowEntryHeader),
        ("flow_id", c_uint32),
        ("src_ip", c_uint32),
        ("dst_ip", c_uint32),
        ("src_port", c_uint16),
        ("dst_port", c_uint16),
        ("base_ts", c_uint64),
        ("_reserved", c_uint32),
        ("options", c_uint8 * 40),  # A 40Byte byte array
    ]

    def split_tcp_option(self):
        offset = 0
        while offset + 1 < len(self.options):
            option_type = self.options[offset]
            if option_type == 0:  # end
                return
            elif option_type == 1:  # padding
                offset += 1
                yield [0x01]
            if offset + 2 < len(self.options):
                length = self.options[offset + 1]
                offset += length
                yield self.options[offset - length : offset]
            else:
                return

    def flow_tuple(self):
        return (
            IPv4Address(self.src_ip),
            self.src_port,
            IPv4Address(self.dst_ip),
            self.dst_port,
        )

    def __repr__(self):
        src_ip, src_port, dst_ip, dst_port = self.flow_tuple()
        return (
            f"TCP[{hex(self.flow_id)}] {src_ip}:{src_port} -> {dst_ip}:{dst_port}, base_ts:{self.base_ts}, options:"
            + " ".join(
                "".join(f"{b:02x}" for b in option)
                for option in self.split_tcp_option()
            )
        )


class RawGeneralPacket(ctypes.LittleEndianStructure):
    _pack = 2
    _fields_ = [
        ("general_pkt", GeneralPktEntry),
        ("flow_index", c_uint16),
        ("_offset_low", c_uint16),
        ("_offset_high", c_uint8),
        ("len", c_uint8),
    ]

    def get_offset(self, chunk_offset=0):
        return chunk_offset + self._offset_low + self._offset_high * 65536

    def __repr__(self):
        return f"RawPacket(flow_index={self.flow_index}), relative_offset:{self.get_offset()},  length: {self.len})"


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


class ChunkEntryHeader(ctypes.LittleEndianStructure):
    _pack_ = 2
    _fields_ = [
        ("length", c_uint16, 12),
        ("type", c_uint16, 4),
    ]

    def __repr__(self):
        return f"ChunkEntryHeader(length={self.length}, type={self.type})"


class ChunkPrologue(ctypes.LittleEndianStructure):
    _pack_ = 2
    _fields_ = [
        ("header", ChunkEntryHeader),
        ("log_version", c_uint32),
        ("data_len", c_uint32),
        ("chunk_len", c_uint32),
        ("offset", c_uint64),
        ("_reserved", c_uint64),
    ]

    def __repr__(self):
        return f"ChunkPrologue type={self.header.type}, version={hex(self.log_version)}, length: {self.data_len}/{self.chunk_len}. offset: {self.offset}"


# an Enum for all possible log entries
class LogEntryVariant:
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

            if GPH.type == 0:  # non-raw General Packet
                if len(entry_buf) < GPH.length:
                    raise ValueError("Too short for GeneralPktEntry")
                GP = GeneralPktEntry.from_buffer(entry_buf)
            elif GPH.type == 1 or GPH.type == 2:
                entry = RawGeneralPacket.from_buffer(entry_buf)
                return entry
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
        elif LH.type == 2:  # Flow
            FLH = FlowEntryHeader.from_buffer(entry_buf)
            if FLH.type == 0:  # TCP/IPv4
                if len(entry_buf) < FLH.length:
                    raise ValueError("Too short for FlowEntry")
                entry = TCPFlowLogEntry.from_buffer(entry_buf)
                return entry
            else:
                print(f"Unsupported GeneralPktHeader type: {FLH.type}")
                return None

        elif LH.type == 15:  # Chunk
            CH = ChunkEntryHeader.from_buffer(entry_buf)
            if CH.type == 0:
                entry = ChunkPrologue.from_buffer(entry_buf)
                return entry
            else:
                print(f"Unsupported ChunkEntryHeader type: {CH.type}")

        else:  # Unsupported log entry type
            return None


def iter_log_entries(buf: bytes):
    offset = 0
    total_len = len(buf)

    chunk_data_end = 1 << 64
    chunk_next = 1 << 64

    while offset + 2 <= total_len:
        LH = LogEntryHeader.from_buffer_copy(buf, offset)
        length = LH.length

        if offset + length > total_len:
            raise ValueError(f"Truncated entry at offset {offset}")

        entry_buf = buf[offset : offset + length]
        entry = LogEntryVariant.from_bytes(bytearray(entry_buf))

        if isinstance(entry, ChunkPrologue):
            chunk_data_end = entry.data_len + offset
            chunk_next = entry.chunk_len + offset
            print(
                f"New chunk at [{offset}, {chunk_data_end}), next expected at {chunk_next}"
            )

        yield entry, length

        offset += length
        if offset >= chunk_data_end:
            offset = chunk_next


class ConvertResultWriter:
    def __init__(self, ip: str):
        self.ip: str = ip
        self.id = None
        self.f_name: str = ""
        self.packets = []

    def __del__(self):
        print(f"Wrote {len(self.packets)} TCP packets to {self.f_name}")
        wrpcapng(self.f_name, self.packets)

    def __repr__(self):
        return (
            f"ConvertResult(ip={self.ip}, id={self.id}, tcp_count={len(self.packets)})"
        )

    def create_writer(self, output_file: str):
        self.f_name = output_file
        self.writer = PcapNgWriter(output_file)

    def add(self, packet, wire_len, timestamp):
        packet.wirelen = wire_len
        packet.time = timestamp
        self.packets.append(packet)


def convert_log_to_pcapng(input_file: str, output_base: str):
    input_path = Path(input_file)
    flow_path = input_path.with_suffix(".flow")
    raw_path = input_path.with_suffix(".raw")

    flow_index: dict[int, int] = {}
    flow_map: dict[int, TCPFlowLogEntry] = {}
    ip_writer_map: dict[str, ConvertResultWriter] = {}

    flow_cnt = 0
    try:
        with open(flow_path, "rb") as f:
            data = f.read()

            for entry, _ in iter_log_entries(data):
                if isinstance(entry, TCPFlowLogEntry):
                    flow_cnt += 1
                    flow_index[flow_cnt] = entry.flow_id
                    flow_map[entry.flow_id] = entry
                    print(entry)
                    src_ip, src_port, dst_ip, dst_port = entry.flow_tuple()
                    src_ip, dst_ip = str(src_ip), str(dst_ip)
                    if src_ip not in ip_writer_map:
                        ip_writer_map[src_ip] = ConvertResultWriter(src_ip)
                    if dst_ip not in ip_writer_map:
                        ip_writer_map[dst_ip] = ConvertResultWriter(dst_ip)
                else:
                    raise ValueError(f"unexpected entry in .flow: {entry}")
    except FileNotFoundError as e:
        print(f"Flow file not found: {e}")
        exit(1)

    # Create pcapng file for each ip
    if output_base.endswith(".pcapng"):
        output_base = output_base[:-7]
    elif output_base.endswith(".pcap"):
        output_base = output_base[:-5]

    for ip in ip_writer_map:
        output_file = f"{output_base}_{str(ip).replace('.', '_')}.pcapng"
        print(f"Create {output_file} for IP {ip}")
        ip_writer_map[ip].create_writer(output_file)

    raw = bytearray()

    try:
        with open(raw_path, "rb") as f:
            raw = f.read()
    except Exception as _e:  # The ".raw" file can be missing, in some modes
        pass

    with open(input_file, "rb") as f_in:
        data = f_in.read()
        chunk_offset = 0
        for entry, used in iter_log_entries(data):
            # print(f"Processing entry: {entry}")
            if isinstance(entry, ChunkPrologue):
                chunk_offset = entry.offset
                continue
            elif isinstance(entry, TCPLogEntry):
                # Use microseconds for ts
                base_ts = flow_map[entry.tcp.flow_id].base_ts // 1000
                pkt_ts = base_ts + entry.general_pkt.ts

                src_ip, src_port, dst_ip, dst_port = flow_map[
                    entry.tcp.flow_id
                ].flow_tuple()

                src_ip, dst_ip = str(src_ip), str(dst_ip)

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

            elif isinstance(entry, RawGeneralPacket):
                flow_id = flow_index[entry.flow_index]
                # Use microseconds for ts
                base_ts = flow_map[flow_id].base_ts // 1000
                pkt_ts = base_ts + entry.general_pkt.ts

                src_ip, src_port, dst_ip, dst_port = flow_map[flow_id].flow_tuple()

                src_ip, dst_ip = str(src_ip), str(dst_ip)
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

                offset = entry.get_offset(chunk_offset)

                if offset + entry.len > len(raw):
                    raise ValueError

                raw_header = raw[offset : offset + entry.len]

                if entry.general_pkt.header.type == 1:
                    # TCP header only, use fake ip
                    ip = IP(
                        src=src_ip,
                        dst=dst_ip,
                        len=entry.general_pkt.pkt_length
                        - 14,  # 14 bytes for Ethernet header
                        id=0,  # Mock data, as we know nothing about IP header
                        flags=0x2,
                        frag=0,
                        chksum=0,
                    )

                    tcp = TCP(raw_header)

                    packet = ether / ip / tcp
                else:
                    ip = IP(raw_header)
                    packet = ether / ip
            else:
                continue

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
                ip_writer_map[ip].add(
                    packet, entry.general_pkt.pkt_length, pkt_ts * 1e-6
                )


def main():
    parser = argparse.ArgumentParser(
        description="Convert Rattan Packet Log file to pcapng file for each end"
    )
    parser.add_argument("input", help="Input Rattan Packet Log file path")
    parser.add_argument(
        "output",
        nargs="?",  # Makes this argument optional (0 or 1 value).
        default=None,
        help="Optional: Output pcapng file name prefix. If omitted, the input file name (without extension) will be used.",
    )

    args = parser.parse_args()

    if args.output is None:
        args.output = args.input

    convert_log_to_pcapng(args.input, args.output)


if __name__ == "__main__":
    main()
