#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "tdigest>=0.5.2",
# ]
# ///

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

# the ip.frag field is used to store the time drift in microseconds
import sys

from tdigest import TDigest

PktAction = {
    0: "Send",
    1: "Recv",
    2: "Drop",
    3: "Passthrough",
}


if len(sys.argv) != 2:
    print("Usage: python get_time_drift.py <.rtl file>")
    sys.exit(1)

rtl_file = sys.argv[1]


stats = {}

with open(rtl_file, "rb") as f:
    # Load 32Bytes each time until EOF
    while True:
        data = f.read(32)
        if not data:
            break

        action = int(data[3] & 0x0F)

        if action != 0:
            continue

        time_drift = int.from_bytes(data[26:28], byteorder="little")
        packet_time = int.from_bytes(data[4:9], byteorder="little")

        flow_id = int.from_bytes(data[12:16], byteorder="little")

        if flow_id in stats:
            stats[flow_id][0].update(time_drift)
            stats[flow_id][1] += 1
        else:
            stats[flow_id] = [TDigest(), 1]
            stats[flow_id][0].update(time_drift)


for flow_id in stats:
    [stat, cnt] = stats[flow_id]

    print(f"Flow {flow_id:08x}, cnt = {cnt}")
    print("Percentiles:")
    print(f"  50th: {stat.percentile(50):.3f} μs")
    print(f"  75th: {stat.percentile(75):.3f} μs")
    print(f"  90th: {stat.percentile(90):.3f} μs")
    print(f"  95th: {stat.percentile(95):.3f} μs")
    print(f"  99th: {stat.percentile(99):.3f} μs")
