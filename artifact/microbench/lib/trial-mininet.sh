#!/usr/bin/env bash

set -euo pipefail

SCENARIO=${1:?usage: trial-mininet.sh <scenario> <log-file>}
LOG=${2:?usage: trial-mininet.sh <scenario> <log-file>}
LIB_DIR="$(dirname "$(realpath "$0")")"

cd "${SCENARIO_DIR:?}"

cmd=(
    taskset -c "${EMULATOR_CORES:?}"
    env
    "LOG_FILE=$LOG"
    "CLIENT_CORES=${CLIENT_CORES:?}"
    "SERVER_CORES=${SERVER_CORES:?}"
    "CLIENT_BARRIER=${CLIENT_BARRIER:?}"
    "SERVER_BARRIER=${SERVER_BARRIER:?}"
    "CCA=${CCA:?}"
    "DURATION=${DURATION:?}"
    python3 "$LIB_DIR/mininet-topology.py" "$SCENARIO.json"
)

if [ -n "${MININET_ISOLATE:-}" ]; then
    exec env NETNS_ROOT=1 "$LIB_DIR/netns-wrapper.sh" "${cmd[@]}"
fi

exec sudo "${cmd[@]}"
