#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=lib.sh
source lib.sh

: "${LOG_TAG:=setup:micro-base}"
require_not_root

log "Growing the root filesystem"
grow_root_fs

log "Installing build tools and the measurement tools the experiments drive"
apt_install \
    build-essential pkg-config git curl ca-certificates rsync \
    iperf3 iproute2 ethtool net-tools psmisc \
    python3 python3-pip python3-matplotlib

log "Installing the current kernel and its headers"
apt_install linux-image-amd64 linux-headers-amd64

# shellcheck disable=SC2012
newest=$(ls -1 /lib/modules | sort -V | tail -1)
if [ "$newest" != "$(uname -r)" ]; then
    log "Running $(uname -r) but $newest is now installed. A reboot is needed."
else
    log "Running $(uname -r), which is the newest installed kernel."
fi
