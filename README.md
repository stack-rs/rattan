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
