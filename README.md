<div align="center">
  <h1>
    <a href="[https://github.com/topgrade-rs/topgrade/releases](https://rattan.stack.rs)"><img alt="Rattan" src="assets/rattan-logo.svg" width="600px"></a>
  </h1>
  <a href="https://github.com/stack-rs/rattan/releases"><img alt="GitHub Release" src="https://img.shields.io/github/release/stack-rs/rattan.svg"></a>
  <a href="https://crates.io/crates/rattan"><img alt="crates.io" src="https://img.shields.io/crates/v/rattan.svg"></a>
  <a href="https://github.com/stack-rs/rattan/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/stack-rs/rattan/actions/workflows/ci.yml/badge.svg"></a>
</div>

Rattan: A High-Performance Modular Transport Channel Emulator Ready for Modern WAN

## Development

### Dependencies

* For tests: `iperf3`, `ethtool` and `iputils-ping`

### Example

```shell
cargo run --example channel --release # AF_PACKET version
cargo run --example channel-xdp --release --features="camellia" # AF_XDP version
```

### Flamegraph

```shell
cargo install flamegraph
cargo flamegraph --root --example channel
```

### Packet Log Spec

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

* `LH` is short for log entry header
* `GPH` is short for general packet entry header
* `GP` is short for general packet entry (the body part)
* `PRH` is short for protocol entry header
* `PRT` is short for protocol entry (the body part)
* `ty.` is short for type
* `ac.` is short for action

```shell
protocol "LH.length:12,LH.ty.:4,GPH.length:8,GPH.ac.:4,GPH.ty.:4,GP.timestamp:32,GP.length:16,PRH.length:12,PRH.ty.:4,PRT (custom):32"
```

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
|             ip.id             |         ip.frag_offset        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          ip.checksum          |   tcp.flags   |    padding    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

```

```shell
protocol "tcp.flow_id:32,tcp.seq:32,tcp.ack:32,ip.id:16,ip.frag_offset:16,ip.checksum:16,tcp.flags:8,padding:8"
```
