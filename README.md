<div align="center">
  <h1>
    <a href="https://github.com/stack-rs/rattan"><img alt="Rattan" src="assets/rattan-logo-slim.svg" width="600px" style="border: none; display: block;"></a>
  </h1>
  <a href="https://github.com/stack-rs/rattan/releases"><img alt="GitHub Release" src="https://img.shields.io/github/release/stack-rs/rattan.svg"></a>
  <a href="https://crates.io/crates/rattan"><img alt="crates.io" src="https://img.shields.io/crates/v/rattan.svg"></a>
  <a href="https://github.com/stack-rs/rattan/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/stack-rs/rattan/actions/workflows/ci.yml/badge.svg"></a>
</div>

**Rattan** is a high-performance modular transport channel emulator ready for modern WAN. We provide a simple and easy-to-use API to create and manage network emulations. Rattan is designed to be used in a wide range of scenarios, from testing network applications to debugging complex network performance issues.

Our modular design makes it easy to extend **Rattan** with different network effects. We provide a set of built-in modules that can be used to emulate different network conditions, such as bandwidth, latency, packet loss, ISP policies and etc.

We support Linux only at the moment. Currently, kernel version v5.4, v5.15, v6.8 and v6.10 are tested.

## Usage

We provide users with a CLI tool to use our pre-defined channels or cells and also a Rust library to build custom channels or cells.

Please check our [User Guide](https://docs.stack.rs/rattan) for how to use **Rattan**.

## Contributing

Rattan is free and open source. You can find the source code on
[GitHub](https://github.com/stack-rs/rattan) and issues and feature requests can be posted on
the [GitHub issue tracker](https://github.com/stack-rs/rattan/issues). Rattan relies on the community to fix bugs and
add features: if you'd like to contribute, please read
the [CONTRIBUTING](https://github.com/stack-rs/rattan/blob/master/CONTRIBUTING.md) guide and consider opening
a [pull request](https://github.com/stack-rs/rattan/pulls).
