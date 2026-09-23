#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=lib.sh
source lib.sh
# shellcheck source=versions.sh
source versions.sh

: "${LOG_TAG:=install:mahimahi}"

if have_cmd mm-link && have_cmd mm-delay && have_cmd mm-loss; then
    log "  already installed"
    exit 0
fi

log "  installing dependencies"
apt_install \
    autoconf automake libtool \
    libxcb1-dev libxcb-composite0-dev libxcb-present-dev \
    libcairo2-dev libpango1.0-dev libssl-dev \
    dnsmasq-base

mkdir -p "$TOOLS_DIR"
clone_at "$MAHIMAHI_REPO" "$MAHIMAHI_COMMIT" "$TOOLS_DIR/mahimahi"

log "  building"
cd "$TOOLS_DIR/mahimahi"
./autogen.sh
./configure
make -j "$(cpu_count)"
sudo make install

sudo sysctl -qw net.ipv4.ip_forward=1

log "  installed $(command -v mm-link)"
