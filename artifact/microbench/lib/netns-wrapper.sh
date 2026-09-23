#!/usr/bin/env bash

set -euo pipefail

[ $# -gt 0 ] || {
    echo "usage: netns-wrapper.sh <command> [args...]" >&2
    exit 1
}

NS="rattan-ae-$$"

cleanup() { sudo ip netns del "$NS" 2>/dev/null || true; }
trap cleanup EXIT

sudo ip netns add "$NS"
sudo ip netns exec "$NS" ip link set lo up

if [ -n "${NETNS_ROOT:-}" ]; then
    sudo ip netns exec "$NS" "$@"
else
    sudo ip netns exec "$NS" runuser -u "$USER" -- "$@"
fi
