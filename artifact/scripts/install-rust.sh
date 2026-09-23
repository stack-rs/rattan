#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=lib.sh
source lib.sh

: "${LOG_TAG:=install:rust}"

if have_cmd cargo; then
    log "  $(cargo --version) already installed"
    exit 0
fi

retry curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
sh /tmp/rustup-init.sh -y --no-modify-path --profile minimal
rm -f /tmp/rustup-init.sh

if ! grep -q '/.cargo/env' "$HOME/.profile" 2>/dev/null; then
    # shellcheck disable=SC2016
    printf '\n. "$HOME/.cargo/env"\n' >>"$HOME/.profile"
fi

# shellcheck source=/dev/null
source "$HOME/.cargo/env"
log "  installed $(cargo --version)"
