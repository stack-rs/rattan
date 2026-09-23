#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=lib.sh
source lib.sh
# shellcheck source=versions.sh
source versions.sh

: "${LOG_TAG:=setup:mptcp-expr}"
require_not_root

APP_DIR="$(realpath ../mptcp/app)"

if [ "$(uname -r)" != "$MPTCP_KERNEL_RELEASE" ]; then
    die "running $(uname -r), not $MPTCP_KERNEL_RELEASE.
  Run 'setup-vm.sh mptcp-kernel' and reboot first."
fi
[ -d /proc/sys/net/mptcp ] ||
    die "$(uname -r) has no /proc/sys/net/mptcp, so it is not the MPTCP kernel."

log "Kernel $(uname -r), MPTCP protocol version $(cat /proc/sys/net/mptcp/mptcp_version)"

log "Installing what the experiment needs"
apt_install \
    build-essential pkg-config git curl ca-certificates rsync \
    iptables iproute2 iputils-ping net-tools \
    python3

log "Installing Rust"
./install-rust.sh

log "Building and installing Rattan"
./build-rattan.sh mptcp

log "Building the transfer application"
# shellcheck source=/dev/null
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
(cd "$APP_DIR" && cargo build --release --locked --quiet)
[ -x "$APP_DIR/target/release/mptcp_app" ] || die "the build produced no mptcp_app"

log "Checking MPTCP is enabled"
sudo sysctl -q -w net.mptcp.mptcp_enabled=1

log "Checking the three subflow schedulers"
for scheduler in default blest ecf; do
    sudo sysctl -q -w "net.mptcp.mptcp_scheduler=$scheduler"
    got=$(cat /proc/sys/net/mptcp/mptcp_scheduler)
    [ "$got" = "$scheduler" ] ||
        die "asked for scheduler '$scheduler', the kernel reports '$got'"
    log "  $scheduler"
done
sudo sysctl -q -w net.mptcp.mptcp_scheduler=default

log "Checking the six congestion control algorithms"
for cca in cubic bbr vegas lia olia balia; do
    sudo modprobe "tcp_$cca" 2>/dev/null || true
    sudo sysctl -q -w "net.ipv4.tcp_congestion_control=$cca"
    got=$(cat /proc/sys/net/ipv4/tcp_congestion_control)
    [ "$got" = "$cca" ] || die "asked for '$cca', the kernel reports '$got'"
    log "  $cca"
done
sudo sysctl -q -w net.ipv4.tcp_congestion_control=cubic

cat <<EOF

Multipath guest ready. Kernel $(uname -r), $(cpu_count) CPUs.

Run the experiment from ~/rattan/artifact/mptcp:

    ./run-benchmark.sh       Table 2

It takes --quick (the default) or --paper. See artifact/README.md.
EOF
