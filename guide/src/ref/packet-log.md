# Packet Log

The Rattan packet log contains the following binary files:

- A `.flow` file, which contains a basic description of all (recognized) flows processed by Rattan.
- A `.rtl` file, which contains a compressed record of the packets in those flows.
- Optionally, a `.raw` file, which contains the protocol headers of those packets.

It is designed to store what happened during the experiment with low overhead, enabling post-analysis and debugging.

**Notice**:
Rattan currently records TCP flows on IPv4 only. Any other packets that are not recognized as a TCP flow on IPv4 are ignored
when recording a Rattan packet log, while they are normally processed and forwarded by
Rattan (e.g., UDP flows).

## Usage

### Record a Rattan packet Log

Two options are involved: `--packet-log` and `--packet-log-mode` . Example usages are:

```bash
rattan link --packet-log packet.rtl --packet-log-mode compact-tcp
rattan run --config config.toml --packet-log packet.rtl --packet-log-mode raw-ip
```

The possible values for `--packet-log-mode` are:

- `compact-tcp` (as default, if not specified). In this mode, some fields from TCP and IP headers are recorded.
- `raw-ip`. In this mode, the raw L3 and L4 headers are recorded.
- `raw-tcp`. In this mode, the raw L4 headers are recorded.

This will create a file named `packet.rtl` in the current directory, which will contain the compressed packet log.
The flow description file will be named `packet.flows`. Also, if any raw headers are recorded, a `packet.raw` will be created.

### Convert a Rattan packet log

#### Rust subcommand

To convert a Rattan packet log to a `.pcapng` file, which can be viewed with Wireshark or other tools, we provide
a subcommand `convert`.

Example usage:

```bash
rattan convert ./packet.rtl [path/to/output]
```

This expects the following files exist, as input:

- `./packet.rtl`
- `./packet.flows`
- `./packet.raw` (optional, only if the packet log was recorded in `raw-tcp` or `raw-ip` mode).

And it will generate the following files, as output, if the address of the two ends of the emulated link
of Rattan was `10.1.1.1` and `10.2.1.1`":

- `path/to/output_10_1_1_1.pcapng`
- `path/to/output_10_2_1_1.pcapng`

If the second argument was not provided (the output prefix), the default value for it is the first argument (the path
to the `.rtl` file).

Note that, in `compact-tcp` mode, some fields are artificially constructed in the pcapng file, as the compressed
packet log only stores partial information. Artificial fields include:

- `Window`: The TCP window size is set to `65535` for all packets.
- `Checksum`: The TCP checksum is set to `0` for all packets.
- `Urgent Pointer`: The TCP urgent pointer is set to `0` for all packets.
- `Options`: The TCP header length is real, but option content is set to `0` for all packets.

Besides, in `raw-tcp` mode, some fields are artificially constructed in the pcapng file, since per-packet
info in IP headers are not recorded at all. Artificial fields include:

- `Identification`: Set to `0`.
- `Flags` : Set to `Don't Fragment`.
- `Checksum` : Set to `0`, or disabled.

#### Python script

We provide a python script ([scripts/log_converter.py](https://github.com/stack-rs/rattan/blob/main/scripts/log_converter.py)), that provides basically the same function as the above-mentioned Rust subcommand does.
Notice that the Python script can be slower than the Rust subcommand by one to
two orders of magnitude when processing large log files.

Example usage:

```bash
python scripts/log_converter.py ./packet.rtl path/to/output
```

or, use the script as a [uv script](https://docs.astral.sh/uv/guides/scripts/#using-a-shebang-to-create-an-executable-file) to automatically handle dependencies.

```bash
uv run --script scripts/log_converter.py ./packet.rtl path/to/output
```

The paths to input and output files are the same as those for `rattan convert`, and the behaviour of the python script is basically the same as `rattan convert`.

## Specification

### Raw file

A `.raw` file is an unstructured binary file. Currently, the headers recorded
in `raw-tcp` and `raw-ip` modes are directly written into the `.raw` file continuously,
without any padding, header or index.
The file is considered a byte buffer, where a slice (starting from `offset` bytes from
the head of the file and ending at `offset` + `length`)
can be indexed by the pair (`offset`, `length`).

### Log entry file

Now we have 2 kinds of log entry files:

- `.flows` files, which contains the metadata for flows. They consist of `Flow Entries`.
- `.rtl` files, which contains per-packet log for packets in those flows.
  - For non-raw modes, they consist of non-raw `General Packet Entries`.
  - For raw modes, they consist of chunks. Each chunk starts with a `Chunk Prologue`, which is followed by raw `General Packet Entries`.

Above mentioned log entry types are described as follows.

Every log entry file consists of `Log Entries`.
Those log entries are stored in system endianness (little-endian in most cases and in our tests).
The specification for a log entry is as follows:

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

Generated with [protocol](https://github.com/luismartingarcia/protocol), where
`LH` is short for log entry header.

`LH.length` denotes the whole length of the log entry file, including the header (2 bytes) and its payload, in bytes.

Currently, there are multiple types of `LH.ty.` that indicate how a log entry's payload is organized. They are:

- `LH.ty.` = 0, for a General Packet Entry as payload.
- `LH.ty.` = 1, reserved for future use.
- `LH.ty.` = 2, for a Flow Entry as payload,
- `LH.ty.` = 15, for a Chunk Prologue as payload,

#### Chunk Prologue Entry

A chunk prologue entry is a log entry with `LH.ty.` = 15, which is used in raw-mode packet logs.

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       LH.length       | LH.ty.|       CPH.length      |CPH.ty.|
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                        CPH.log_version                        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                        CPH.data_length                        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                        CPH.chunk_length                       |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                                                               +
|                                                               |
+                           CP (custom)                         +
|                                                               |
+                                                               +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

```bash
protocol "LH.length:12,LH.ty.:4,CPH.length:12,CPH.ty.:4,CPH.log_version:32,CPH.data_length:32,CPH.chunk_length:32,CP (custom):128"
```

Where `CPH` is short for Chunk Prologue Header, and `CP` is short for Chunk Prologue.
The meaning of each field is as follows:

- `CPH.length` denotes the length of the chunk prologue entry itself.
- `CPH.ty.` is always 0 now.
- `CPH.log_version` is a magic number, which is 0x20260101 in this specification.
- `CPH.data_length` is the sum of the length of this Chunk Prologue Entry and all subsequent content (which, for now, consists solely of raw General Packet Entries).
- `CPH.chunk_length` is the logical length of the current chunk. It indicates that the next Chunk Prologue is expected to be found, or the End-of-File (EOF) is expected to be reached, exactly `CPH.chunk_length` bytes from the offset of this log entry.

And when `CPH.ty.` is 0, the body part of the Chunk Prologue is:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                           CP.offset                           +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                            Reserved                           +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

```bash
protocol "CP.offset:64,Reserved:64"
```

Where `CP` is short for Chunk Prologue.
`CP.offset` is a 64-bit integer, whose value is referenced by the raw General Packet entries in the chunk that this
Chunk Prologue introduces.

#### General Packet Entry

A General Packet Entry is a log entry with `LH.ty.` = 0, which records a packet.

There are two modes of General Packet Entries:

- non-raw (`GPH.ty.` = 0), and
- raw (`GPH.ty.` = 1, 2).

**non-raw General Packet Entry**:
The specification for a non-raw General Packet Entry:

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
- `GP` is short for general packet entry (the body part).
- `PRH` is short for protocol entry header.
- `PRT` is short for protocol entry (the body part).
- `ty.` is short for type.
- `ac.` is short for action.

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

**raw General Packet Entry**:
The specification for a raw General Packet Entry:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       LH.length       | LH.ty.|   GPH.length  |GPH.ac.|GPH.ty.|
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          GP.timestamp                         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|           GP.length           |         RGP.flow_index        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|              RGP.relative_offset              |    RGP.len.   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

```bash
protocol "LH.length:12,LH.ty.:4,GPH.length:8,GPH.ac.:4,GPH.ty.:4,GP.timestamp:32,GP.length:16,RGP.flow_index:16,RGP.relative_offset:24,RGP.len.:8"
```

The `GP` and `GPH` fields have the same meaning as those in non-raw General Packet Entries.
And `RGP` is short for raw General Packet.

For raw General Packet Entries, `GPH.ty.` could be one of:

- `GPH.ty.` = 1, for raw packet logs with raw L3 and L4 headers (`raw-ip`).
- `GPH.ty.` = 2, for raw packet logs with raw L4 headers (`raw-tcp`).

And those are descriptions for other fields that will be only used in raw General Packet Entries:

If `RGP.flow_index` is 0, it is a default value for packets that can not be
related to a recorded flow for any reason. Otherwise, it means the index of flow in the `.flow` file (counting from 1), that this packet belongs to. It is used, rather
than directly the 32-bit flow id, for a more compact and cache line-friendly representation.

Instead of recording the variable-length raw headers of the packet directly in the raw mode General Packet Entry, we record them in an indirect way:

The raw headers are continuously written into the `.raw` file, and the length of the headers of a certain packet can be located in the `.raw` file by the byte range `[start, start + length)`. Both the `start` and `length` are recorded in the `.rtl` file in the following way:

The `start` is recorded as `CP.offset` + `RGP.relative_offset`. Here, `CP.offset` is the value of that field in the Chunk Prologue of the current chunk,
and `RGP.relative_offset` is the value of that field in the current raw General Packet Entry.
Notably, `CP.offset` is a 64-bit unsigned integer and `RGP.relative_offset` is a 24-bit unsigned integer. This design prevents the overflow of the resulting `start` offset while maintaining a compact representation for the raw General Packet Entry.

The `length` is recorded as `RGP.len` in the current raw General Packet Entry. The `length` is a 8-bit unsigned integer, which means that the maximum allowed length of the raw headers recorded by a raw General Packet Entry is 255 bytes.

#### Flow Entry

A flow entry is a log entry with `LH.ty.` = 2, which records basic descriptive info for a flow.

A "flow" is unidirectional in Rattan Packet Log.
For example, a single connection between a TCP client and TCP server is considered as 2 flows.

Currently there are only one type of flows supported, the TCP/IPv4 flow, and the Flow entries for those flows, stored continuously in the `.flow` file, have a specification as follows:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       LH.length       | LH.ty.|       FLH.length      |FLH.ty.|
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                             FLE.id                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                             src.ip                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                             dst.ip                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|            src.port           |            dst.port           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                       FLE.base_timestamp                      +
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
+                      FLE.tcp_syn_options                      +
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
protocol "LH.length:12,LH.ty.:4,FLH.length:12,FLH.ty.:4,FLE.id:32,src.ip:32,dst.ip:32,src.port:16,dst.port:16,FLE.base_timestamp:64,Reserved:32,FLE.tcp_syn_options:320"
```

Where `FLH` is short for Flow Entry Header, and `FLE` is short for Flow Entry.
The meaning of each field is as follows:

- `FLH.length` is the length for the whole flow entry.
- `FLH.ty.` must be 0, denoting a TCP/IPv4 flow.
- `FLE.id`: The identifier of the flow. The high 16 bits are the interface ID where the packet enters Rattan, and the low 16 bits are the incremental flow counter.
- `FLE.base_timestamp`: The base timestamp in nanoseconds (this unit is different from the `GP.timestamp` field) for this flow.
- `src.ip`, `dst.ip`, `src.port`, `dst.port`: The source IPv4 address, destination IPv4 address, source TCP port number, destination TCP port number.
- `FLE.tcp_syn_options`: As flows are unidirectional here, only one valid SYN (TCP client side) or SYN_ACK (TCP server side) packet would exist in a TCP connection. The TCP options fields in such packets are recorded in raw here. This field has a fixed length of 40 bytes, and zero-padding at the end if the tcp options are less than 40 bytes. 40 bytes are the upper limit in TCP protocol: a TCP header can be at most 15
\* 4 = 60 bytes in length, and the first 20 bytes are used for fixed fields in the TCP header.