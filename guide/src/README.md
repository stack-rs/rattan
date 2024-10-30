<div align="center">
  <h1>
    <a href="https://github.com/stack-rs/rattan"><img alt="Rattan" src="assets/rattan-logo-slim.svg" width="600px" style="border: none; display: block;"></a>
  </h1>
  <a href="https://github.com/stack-rs/rattan/releases"><img alt="GitHub Release" src="https://img.shields.io/github/release/stack-rs/rattan.svg"></a>
  <a href="https://crates.io/crates/rattan"><img alt="crates.io" src="https://img.shields.io/crates/v/rattan.svg"></a>
  <a href="https://github.com/stack-rs/rattan/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/stack-rs/rattan/actions/workflows/ci.yml/badge.svg"></a>
</div>

# Introduction

**Rattan** is a high-performance modular transport channel emulator ready for modern WAN. We provide a simple and easy-to-use API to create and manage network emulations. Rattan is designed to be used in a wide range of scenarios, from testing network applications to debugging complex network performance issues.

Our modular design makes it easy to extend **Rattan** with different network effects. We provide a set of built-in modules that can be used to emulate different network conditions, such as bandwidth, latency, packet loss, ISP policies and etc.

We support Linux only at the moment. Currently, kernel version v5.4, v5.15, v6.8 and v6.10 are tested.

## Design Targets

- **Flexible**. Rattan is agnostic of path emulation model.
- **Fast**. Rattan provides both high peak performance and execution efficiency.
- **Extensible**. Rattan provides rich features out-of-the-box but easily extensible for custom conditions.
- **User-Friendly**. Rattan provides simple and intuitive interfaces for quick usage but also ensure it's fully controllable under the hood.

## Basic Concepts

Run `rattan` will generate some network namespaces and veth pairs to
emulate the network environment. The network topology is like:

```txt
   ns-left                                                           ns-right
+-----------+ [Internet]                               [Internet] +-----------+
|    vL0    |    |                 ns-rattan                 |    |    vR0    |
|  external | <--+          +---------------------+          +--> |  external |
|   vL1-L   |               |  vL1-R  [P]   vR1-L |               |   vR1-R   |
| .1.1.x/32 | <-----------> |.1.1.2/32   .2.1.2/32| <-----------> | .2.1.x/32 |
|   vL2-L   |               |  vL2-R        vR2-L |               |   vR2-R   |
| .1.2.y/32 | <-----------> |.1.2.2/32   .2.2.2/32| <-----------> | .2.2.y/32 |
~    ...    ~   Veth pairs  ~  ...           ...  ~   Veth pairs  ~    ...    ~
+-----------+               +---------------------+               +-----------+
```

Then Rattan will build and link the cells according to the configuration file.
Each cell emulates some network characteristics, such as bandwidth, delay, loss, etc.

**ATTENTION**: Check firewall settings before running Rattan CLI.
Please make sure you allow the following addresses:

- 10.1.1.0/24
- 10.2.1.0/24

For example, you can run the following commands if using `ufw`:

- `ufw allow from 10.1.1.0/24`
- `ufw allow from 10.2.1.0/24`

## Contributing

Rattan is free and open source. You can find the source code on
[GitHub](https://github.com/stack-rs/rattan) and issues and feature requests can be posted on
the [GitHub issue tracker](https://github.com/stack-rs/rattan/issues). Rattan relies on the community to fix bugs and
add features: if you'd like to contribute, please read
the [CONTRIBUTING](https://github.com/stack-rs/rattan/blob/master/CONTRIBUTING.md) guide and consider opening
a [pull request](https://github.com/stack-rs/rattan/pulls).
