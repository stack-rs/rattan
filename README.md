# [![Rattan](assets/rattan-logo.svg)](https://rattan.stack.rs)

[![CI](https://github.com/stack-rs/rattan/actions/workflows/ci.yml/badge.svg)](https://github.com/stack-rs/rattan/actions/workflows/ci.yml)

High Performance Modular Transport Channel Emulator Ready for Post-Gigabit Era.

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
