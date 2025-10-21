# Installation

The Rattan project contains a CLI tools (named `rattan`) for common use cases and
a Rust library (named `rattan-core`) for you to build your own tools on top of.

There are multiple ways to install the Rattan CLI tool.
Choose any one of the methods below that best suit your needs.

> [!CAUTION]
> As we are still in the early stages of development without a released version, we only support building from source currently.

## Pre-compiled binaries

Executable binaries are available for download on the [GitHub Releases page][releases].
Download the binary and extract the archive.
The archive contains an `rattan` executable which you can run to start your distributed platform.

To make it easier to run, put the path to the binary into your `PATH` or install it in a directory that is already in your `PATH`.
Note that, you have to grant the binary the necessary capabilities to run.
For example, you can run the following commands on Linux:

```bash
# install the binary
sudo install -m 755 rattan /usr/local/bin/rattan
# grant the necessary capabilities
sudo setcap 'cap_dac_override,cap_dac_read_search,cap_sys_ptrace,cap_net_admin,cap_sys_admin,cap_net_raw+ep' /usr/local/bin/rattan
```

Besides, for certain operating systems that enable `systemd-networkd`, you have to configure it to not change MAC address of veth interfaces.
You can do this by adding the following lines to `/lib/systemd/network/80-rattan.link`:

```ini
[Match]
OriginalName=ns*-v*-*
Driver=veth

[Link]
MACAddressPolicy=none
```

And then restart the `systemd-networkd` service:

```bash
sudo systemctl daemon-reload
sudo systemctl restart systemd-networkd.service
```

We also contain a install script [scripts/install.sh](https://github.com/stack-rs/rattan/blob/main/scripts/install.sh) in the repository, which you can use to install the binary on Ubuntu:

```bash
./scripts/install.sh rattan
```

[releases]: https://github.com/stack-rs/rattan/releases

## Build from source using Rust

### Dependencies

We recommend developers to install the following dependencies for better testing and development experience. And you have to install some of the dependencies to build rattan with specific features:

```bash
sudo apt install ethtool iputils-ping iperf3 pkg-config m4 clang llvm libelf-dev libpcap-dev gcc-multilib
```

### Installing with Cargo

To build the `rattan` executable from source, you will first need to install Rust and Cargo.
Follow the instructions on the [Rust installation page].

Once you have installed Rust, the following command can be used to build and install rattan:

```bash
cargo install rattan
```

This will automatically download rattan from [crates.io], build it, and install it in Cargo's global binary directory (`~/.cargo/bin/` by default).

You can run `cargo install rattan` again whenever you want to update to a new version.
That command will check if there is a newer version, and re-install rattan if a newer version is found.

To uninstall, run the command `cargo uninstall rattan`.

[Rust installation page]: https://www.rust-lang.org/tools/install
[crates.io]: https://crates.io/

### Installing the latest git version with Cargo

The version published to crates.io will ever so slightly be behind the version hosted on GitHub.
If you need the latest version you can build the git version of rattan yourself.
Cargo makes this **_super easy_**!

```bash
cargo install --git https://github.com/stack-rs/rattan.git rattan
```

Again, make sure to add the Cargo bin directory to your `PATH`.

### Building from source

If you want to build the binary from source, you can clone the repository and build it using Cargo.

```bash
git clone https://github.com/stack-rs/rattan.git
cd rattan
cargo build --release
```

Then you can find the binary in `target/release/rattan` and install or run it as you like (e.g., run `./scripts/install.sh target/release/rattan`).

## Modifying and contributing

If you are interested in making modifications to Rattan itself, check out the [Contributing Guide] for more information.

[Contributing Guide]: https://github.com/stack-rs/mitosis/blob/master/CONTRIBUTING.md
