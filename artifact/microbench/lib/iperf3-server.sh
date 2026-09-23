#!/usr/bin/env bash

set -euo pipefail

SERVER_CORES=${SERVER_CORES:-0-$(($(getconf _NPROCESSORS_ONLN) - 1))}
SERVER_BARRIER=${SERVER_BARRIER:?SERVER_BARRIER must be set}
DURATION=${DURATION:-10}

timeout $((DURATION * 4 + 30)) taskset -c "$SERVER_CORES" iperf3 --server --one-off >/dev/null 2>&1 &
server=$!

sleep 0.5
: >"$SERVER_BARRIER"

if [ -n "${NO_WAIT:-}" ]; then
    exit 0
fi

wait "$server" || true
