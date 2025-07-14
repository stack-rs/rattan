# Packet Log

The packet log contains a binary file that contains a compressed record of all packets processed by Rattan and a flow description file. It is designed to store what happened during the experiment with low overhead, enabling post-analysis and debugging.

## Usage

Using the `--packet-log` option in the CLI will enable the packet log feature and specify the file to store the records. The flow description file will be stored in the same directory with the name `<packet_log>.flows`, where `<packet_log>` is the name of the packet log file.

For example, you can run the following command to enable packet logging:

```bash
rattan link --packet-log packet.log
```

This will create a file named `packet.log` in the current directory, which will contain the compressed packet log. The flow description file will be named `packet.log.flows`.

We also provide a python script [scripts/log_converter.py](https://github.com/stack-rs/rattan/blob/main/scripts/log_converter.py) to convert the packet log file (along with the flow description file) to some pcapng files, which can be viewed with Wireshark or other tools. You can run the script like this:

```bash
python3 scripts/log_converter.py packet.log converted.pcapng
```

This will read `packet.log` and `packet.log.flows`, and generate a pcapng file for each end of Rattan (identified by IP address), with the name `converted_<ip>.pcapng` (e.g. `converted_10_1_1_1.pcapng`).

Note that some fields are artificially constructed in the pcapng file, as the compressed packet log only stores partial information. Artificial fields include:

- `Window`: The TCP window size is set to `65535` for all packets.
- `Checksum`: The TCP checksum is set to `0` for all packets.
- `Urgent Pointer`: The TCP urgent pointer is set to `0` for all packets.
- `Options`: The TCP header length is real, but option content is set to `0` for all packets.

## Specification

The packet log is stored in system endianness (little-endian in most cases and in our tests), entry by entry. The specification for each log entry is as follows:

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

Generated with [protocol](https://github.com/luismartingarcia/protocol), where:

- `LH` is short for log entry header
- `GPH` is short for general packet entry header
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
- `LH.ty.`: Type of the log entry, currently only `0` is supported (indicating a general packet log entry).
- `GPH.length`: Length of the general packet entry in bytes, including the header and body (`GPH` + `GP`). It is 8 bytes in most cases.
- `GPH.ac.`: Action of the general packet entry. It can be one of the following values:
  - `0`: Send (packet leaves Rattan)
  - `1`: Receive (packet enters Rattan)
  - `2`: Drop (packet is dropped by Rattan)
  - `3`: Passthrough (packet passes through Rattan without any effect applied)
- `GPH.ty.`: Type of the general packet entry, currently only `0`.
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

For TCP log, each line of the flow description file will contain a JSON object with the following fields:

```json
{"flow_id":<flow_id>,"base_ts":<base_ts>,"flow_desc":{"TCP":["<src_ip>","<dst_ip>",<src_port>,<dst_port>]}}
```

Where:

- `flow_id`(`u32`): The identifier of the flow. The high 16 bits are the interface ID where the packet enters Rattan, and the low 16 bits are the incremental flow counter.
- `base_ts`(`i64`): The base timestamp in nanoseconds (this unit is different from the `GP.timestamp` field) for this flow.
- `flow_desc`: The description map of the flow, which contains the protocol-specific information. For TCP, it contains a key `TCP` with a list of four elements:
  - The source IP address (`Ipv4Addr`)
  - The destination IP address (`Ipv4Addr`)
  - The source port (`u16`)
  - The destination port (`u16`)
