#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=lib.sh
source lib.sh

: "${LOG_TAG:=setup:micro}"
require_not_root

if [ ! -d "/lib/modules/$(uname -r)/build" ]; then
    die "no kernel headers for $(uname -r). Run 'setup-vm.sh micro-base' and reboot first."
fi

log "Installing Rust"
./install-rust.sh

log "Building and installing Rattan"
./build-rattan.sh micro

log "Installing Mahimahi"
./install-mahimahi.sh

log "Installing Mininet"
./install-mininet.sh

cat <<EOF

Microbenchmark guest ready. Kernel $(uname -r), $(cpu_count) CPUs.

Run the experiments from ~/rattan/artifact/microbench:

    ./run-throughput.sh      Figure 5
    ./run-complexity.sh      Figure 6
    ./run-density.sh         Figure 7

Each takes --quick (the default) or --paper. See artifact/README.md.
EOF
