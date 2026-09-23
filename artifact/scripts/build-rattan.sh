#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=lib.sh
source lib.sh

: "${LOG_TAG:=build:rattan}"
REPO="$HOME/rattan"

[ $# -eq 1 ] || die "usage: build-rattan.sh <micro|mptcp>"
target=$1

# shellcheck source=/dev/null
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
require_cmd cargo

cd "$REPO"

case $target in
micro)
    log "  cargo build --release --locked --features rvnic"
    cargo build --release --locked --features rvnic
    log "  installing rattan and rattan-rv"
    scripts/install.sh target/release/rattan
    scripts/install.sh target/release/rattan-rv

    log "  building the RVNIC kernel module"
    make -C rvnic/kernel
    sudo make -C rvnic/kernel install
    ;;
mptcp)
    log "  cargo build --release --locked --features first-payload"
    cargo build --release --locked --features first-payload
    log "  installing rattan"
    scripts/install.sh target/release/rattan
    ;;
*)
    die "unknown target '$target'. Expected micro or mptcp."
    ;;
esac

log "  $(rattan --version)"
