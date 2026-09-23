#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=scripts/lib.sh
source scripts/lib.sh

usage() {
    cat >&2 <<'EOF'
Prepare this guest for one group of experiments.

Usage: setup-vm.sh <stage>

Stages:
  micro-base     system packages and the current kernel plus its headers, for
                 the microbenchmark guest (§5.1). Reboot afterwards.
  micro          everything else §5.1 needs: Rattan, the RVNIC kernel module,
                 Mahimahi and Mininet. Run this after rebooting.
  mptcp-kernel   build and install the out-of-tree MPTCP v0.96 kernel, then
                 point GRUB at it. Reboot afterwards.
  mptcp-expr     everything else §5.2 needs: Rattan and the transfer
                 application. Run this after rebooting into the MPTCP kernel.
EOF
    exit 1
}

[ $# -eq 1 ] || usage

case $1 in
micro-base) LOG_TAG=setup:micro-base scripts/setup-micro-base.sh ;;
micro) LOG_TAG=setup:micro scripts/setup-micro.sh ;;
mptcp-kernel) LOG_TAG=setup:mptcp-kernel scripts/setup-mptcp-kernel.sh ;;
mptcp-expr) LOG_TAG=setup:mptcp-expr scripts/setup-mptcp-expr.sh ;;
-h | --help) usage ;;
*) die "unknown stage '$1'. Expected micro-base, micro, mptcp-kernel or mptcp-expr." ;;
esac
