# Packet Log

The packet log contains the following binary files:

- A `.flow` file, which contains a basic describe of all (recognized) flows processed by Rattan.
- A `.rtl` file, which contains a compressed record of the packets in those flows.
- Optionally, a `.raw` file, which contains the protocol headers of those packets.

It is designed to store what happened during the experiment with low overhead, enabling post-analysis and debugging.

**Notice**:
Rattan only records TCP flows on Ipv4 only. Any other packets that is not recognized as a TCP flow on Ipv4 is ignored
when recording a rattan packet log, while they are normally processed and forwarded by
rattan (e.g., Udp flows).

## Usage

### Record a Rattan packet Log

Two options are involved: `--packet-log` and `--packet-log-mode` . Example usages are:

```bash
rattan link --packet-log packet.rtl --packet-log-mode compact-tcp
rattan run --config CONFIG_FILE --packet-log packet.rtl --packet-log-mode raw-ip
```

The possible values for `--packet-log-mode` are :

- `compact-tcp` (as default, if not specified). In this mode, some fields from TCP and IP headers are recorded.
- `raw-ip`. In this mode, the raw L3 and L4 headers are recorded.
- `raw-tcp`. In this mode, the raw L4 headers are recorded.

This will create a file named `packet.rtl` in the current directory, which will contain the compressed packet log.
The flow description file will be named `packet.flows`. Also, if any raw headers are recorded, a `packet.raw` will be created.

### Convert a Rattan packet log

**Rust subcommand**

To convert a rattan packet log to `.pcapng` file, which can be viewed with Wireshark or other tools, we provide
a subcommand `convert`.

Example usage:

```bash
rattan convert ./packet.rtl [./output]
```

This expects the following files exist, as input:

- `./packet.rtl`
- `./packet.flows`
- `./packet.raw` (optional, only if the packet log was recorded in `raw-tcp` or `raw-ip` mode).

And it will Generated the following files, as output, if the address of the two ends of the emulated link
of Rattan was `10.1.1.1` and `10.2.1.1`"

- `./output_10_1_1_1.pcapng`
- `./output_10_2_1_1.pcapng`

If the second argumnet was not provided(the output prefix), the default value for it is the first argument (the path
to the `.rtl` file).

Note that, in `compact-tcp` mode, some fields are artificially constructed in the pcapng file, as the compressed
packet log only stores partial information. Artificial fields include:

- `Window`: The TCP window size is set to `65535` for all packets.
- `Checksum`: The TCP checksum is set to `0` for all packets.
- `Urgent Pointer`: The TCP urgent pointer is set to `0` for all packets.
- `Options`: The TCP header length is real, but option content is set to `0` for all packets.

Besides, in `raw-tcp` mode, some fields are artifically constructed in the pcapng file, since per-packet
info in IP headers are not recorded at all. Artificial fields include:

- `Identification`: Set to `0`.
- `Flags` : Set to `Don't Fragment`.
- `Checksum` : Set to `0`, or disabled.

**Python subcommand**

We do provide a python script (scripts/log_converter.py), that provide basically the same function of
transforming a Rattan packet log to `.pcapng` files. It is slower than the rust subcommand for one to
two orders of magnitude when processing large log files.

Example usage:

```bash
python scripts/log_converter.py ./packet.rtl ./output
```

The path to input and output files are the same as those for `rattan convert`.

The behaviour of the python script is basically the same as `rattan convert`, except for that the output path prefix
is not optional.

## Specification

### Raw file

A `.raw` file is an unstructed binary file.
Currently, the headers recorded in `raw-tcp` and `raw-ip` modes are directly written into the `.raw` file continuously,
without any padding, header or index.
The file is considered as a byte buffer, that a slice in it can be indexed by the
pair (`offset`, `length`).

### Log entry file

Now we have 2 kinds of log entry files:

- `.flows` files, which contains the metadata for flows. They consist of `Flow Entries`.
- `.rtl` files, which contains per-packet log for packets in those flows.
  - For non-raw modes, they consist of non-raw `General Packet` Entries.
  - For now modes, they consist of chunks. Each chunk starts with a `Chunk Prologue` and following raw `General Packet` Entries.

Above mentioned log entry types are described as follows.

Every log entry file consists of `Log Enties`.
Those log entries are stored in system endianness (little-endian in most cases and in our tests). The specification
for a log entry is as follows:

```text
0                   1                   2                   3
0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       LH.length       | LH.ty.|                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+                               +
|                                                               |
+                                                               +
|                            payload                            |
+                                                               +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

```bash
protocol "LH.length:12,LH.ty.:4,payload:112"
```

Generated with [protocol](https://github.com/luismartingarcia/protocol), where:

- `LH` is short for log entry header

`LH.length` denotes the whole length of the log entry file, including the header(2 Bytes) and its payload, in Bytes.

Now there are mutiple possibility of `LH.ty.` that decides how the payload of an
log entry should be interpreted. They are:

- `LH.ty` = 0, for a "General Packet Entry" as payload.
- `LH.ty` = 1, reserved for furture use.
- `LH.ty` = 2, for a "Flow Entry" as payload,
- `LH.ty` = 15, for a "Chunk Header" as payload,

#### General Packet entry

A Gerneral Packet entry is a log entry with `LH.ty.` = 0, which records a packet.

There are two types of general packet entries:

- non-raw (`GPH.ty` = 0), and
- raw mode (`GPH.ty` = 1, 2).

**non-raw Gerneral Packet Entry**:
The specification for a non-raw general packet entry:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       LH.length       | LH.ty.|   GPH.length  |GPH.ac.|GPH.ty.|
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          GP.timestamp                         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|           GP.length           |       PRH.length      |PRH.ty.|
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          PRT (custom)                         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

- `GPH` is short for general packet entry header.
- `GP` is short for general packet entry (the body part)
- `PRH` is short for protocol entry header
- `PRT` is short for protocol entry (the body part)
- `ty.` is short for type
- `ac.` is short for action

```bash
protocol "LH.length:12,LH.ty.:4,GPH.length:8,GPH.ac.:4,GPH.ty.:4,GP.timestamp:32,GP.length:16,PRH.length:12,PRH.ty.:4,PRT (custom):32"
```

The meaning of each field is as follows:

- `LH.length`: Length of the whole log entry in bytes. For TCP log entries, it is 32 bytes.
- `LH.ty.`: Type of the log entry, currently only `0` is supported (indicating a general packet entry).
- `GPH.length`: Length of the general packet entry in bytes, including the header and body (`GPH` + `GP`). It is 8 bytes in most cases.
- `GPH.ac.`: Action of the general packet entry. It can be one of the following values:
  - `0`: Send (packet leaves Rattan)
  - `1`: Receive (packet enters Rattan)
  - `2`: Drop (packet is dropped by Rattan)
  - `3`: Passthrough (packet passes through Rattan without any effect applied)
- `GPH.ty.`: Type of the general packet entry, `0` for non-raw GPH.
- `GP.timestamp`: Timestamp of the packet entry in microseconds since `base_ts` (in flow description file).
- `GP.length`: Length of the whole packet in bytes.
- `PRH.length`: Length of the protocol entry in bytes, including the header and body (`PRH` + `PRT`). For TCP log entries, it is 22 bytes.
- `PRH.ty.`: Type of the protocol entry, currently only `0` is supported (indicating a TCP log entry).
- `PRT`: The body part of the protocol entry, which contains the protocol-specific information.

We currently provide TCP log spec (a variant of the protocol entry body part, i.e. `PRT (custom)`):

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          tcp.flow_id                          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                            tcp.seq                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                            tcp.ack                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|             ip.id             |            ip.frag            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          ip.checksum          |   tcp.flags   |  tcp.dataofs  |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

```

```bash
protocol "tcp.flow_id:32,tcp.seq:32,tcp.ack:32,ip.id:16,ip.frag:16,ip.checksum:16,tcp.flags:8,tcp.dataofs:8"
```

Note that the `ip.frag` includes the fragment offset and the flags. Other fields are self-explanatory.

**raw Gerneral Packet Entry**:
The specification for a raw general packet entry:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       LH.length       | LH.ty.|   GPH.length  |GPH.ac.|GPH.ty.|
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          GP.timestamp                         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|           GP.length           |           Flow Index          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|            Relative Offset to Chunk           |   Record.len  |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

```bash
protocol "LH.length:12,LH.ty.:4,GPH.length:8,GPH.ac.:4,GPH.ty.:4,GP.timestamp:32,GP.length:16,Flow Index:16,Relative Offset to Chunk:24,Record.len:8"
```

The `GP` and `GPH` files has the same meaning as the non-raw Genreal Packet Entries.
For raw General Packet Entries, `GPH.ty` could be one of:

- `GPH.ty` = 1, for raw packet logs with raw L3 and L4 headers (`raw-ip`).
- `GPH.ty` = 2, for raw packet logs with raw L4 headers (`raw-tcp`).

And those are descriptions for other fields that the non-raw ones does not have:

If `Flow Index` is 0, it is a default value for packets that can not be
related to a recorded flow for any reason. Otherwise, it means the index of flow in the `.flow` file (counting from 1), that this packet belongs to. It is used, rather
than directly the 32-bit flow id, is for a more compact and cache line-friendly representation.

The `Relative Offset to Chunk` and `Record.len` is a pointer to the `.raw` file, pointing at the raw header of the packet. Such raw-mode General Packet Entry is
always part of a chunk, leading by a `Chunk Prologue`.
If the `Offset` in the chunk prologue that this raw general packet entry follows is `offset_chunk`, and (`Relative Offset to Chunk` , `Record.len`) = (`offset_entry`, `len`), the header of the packet is stored in such byte slice in the `.raw` file:

- starts at the Byte offset `offset_chunk` + `offset_enty`.
- ends at the Byte offset `offset_chunk` + `offset_enty` + `len`.

#### Flow Entry

A flow entry is a log entry with `LH.typ` = 2, which records basic descriptive info for a flow.

A "flow" is uni-directional here. For example, a single connection between a
TCP client and TCP server is considered as 2 flows.

Currently there are only one type of flows supported, the TCP/IPv4 flow, and the Flow entries for those flows, stored continuously in the `.flow` file, have a specification as follows:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       LH.length       | LH.ty.|       FLE.length      |FLE.ty.|
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                            Flow Id                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                             src.ip                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                             dst.ip                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|            src.port           |            dst.port           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                         Base Timestamp                        +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                            Reserved                           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                                                               +
|                                                               |
+                                                               +
|                                                               |
+                                                               +
|                                                               |
+                                                               +
|                                                               |
+                     Options on SYN/SYN_ACK                    +
|                                                               |
+                                                               +
|                                                               |
+                                                               +
|                                                               |
+                                                               +
|                                                               |
+                                                               +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

```bash
protocol "LH.length:12,LH.ty.:4,FLE.length:12,FLE.ty.:4,Flow Id:32,src.ip:32,dst.ip:32,src.port:16,dst.port:16,Base Timestamp:64,Reserved:32,Options on SYN/SYN_ACK:320"
```

where

- `FLE` denotes Flow Entry. `FLE.length` is the length for the whole flow entry. `FLE.type` could be 0 only for now, denoting a TCP/IPv4 flow.
- `Flow id`: The identifier of the flow. The high 16 bits are the interface ID where the packet enters Rattan, and the low 16 bits are the incremental flow counter.
- `Base Timestamp`: The base timestamp in nanoseconds (this unit is different from the `GP.timestamp` field) for this flow.
- `src.ip`, `dst.ip`, `src.port`, `dst.port`: The source IPv4 address, destination IPv4 address, source TCP port number, description TCP port number.
- `Options on SYN/SYN_ACK`: As flows are uni-directional here, only one valid of SYN(TCP client side) or SYN_ACK (TCP server side) would exist in a TCP connection. The TCP options fileds in such pakcets are recorded in raw here. This field has a fixed length of 40 Bytes, and zero-padding at the end if the tcp options as less than 40 Bytes. 40 Bytes is the upper limit in TCP protocol: a TCP header can be at most 15 \* 4 = 60 Bytes in length, and the first 20 bytes are used for fixed fields in the TCP header.

#### Chunk Prologue Entry

A chunk prologue entry is a log entry with `LH.typ` = 15, which is used in raw-mode packet logs.

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       LH.length       | LH.ty.|       CH.length       | CH.ty.|
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          Log Version                          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          Data Length                          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          Chunk Length                         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                                                               +
|                                                               |
+                         custom payload                        +
|                                                               |
+                                                               +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

```bash
protocol "LH.length:12,LH.ty.:4,CH.length:12,CH.ty.:4,Log Version:32,Data Length:32,Chunk Length:32,custom payload:128"
```

where:

- `CH` is short for Chunk Prologue Header, and `CH.length` denotes the length for the chunk prologue entry, and `CH.ty` is always 0 for now.
- `Log Version` is a magic number of `0x20251120`.
- `Data Length` is the sum of the length of this Chunk Prologue Entry and all the following contents (raw General Packet Entry) for now.
- `Chunk Length` is the logical length of the current chunk, i.e., it is expected to either meet EOF or to find the next Chunk Prologue, `Chunk Length` bytes since the offset of this log entry.

And the custom payload for the `CH.ty` = 0 is:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                             Offset                            +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                            Reserved                           +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

```bash
protocol "Offset:64,Reserved:64"
```

Where `Offset` is a 64-bit integer, and the value is used for the raw General Packet entries. It is a offset in the `.raw` file.
