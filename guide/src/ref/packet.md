# Packet I/O

Currently, we support two packet I/O mechanisms: `AF_PACKET` and `AF_XDP`. The former is a socket-based mechanism, while the latter is a driver-based mechanism. The `AF_PACKET` version is more portable, while the `AF_XDP` version is more performant.

## Example

By default, the `AF_PACKET` version is used. To use the `AF_XDP` version, you need to enable the `camellia` feature.

```bash
cargo run --example channel --release # AF_PACKET version
cargo run --example channel-xdp --release --features="camellia" # AF_XDP version
```
