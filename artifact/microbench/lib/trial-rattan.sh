#!/usr/bin/env bash

set -euo pipefail

SCENARIO=${1:?usage: trial-rattan.sh <scenario> <log-file>}
LOG=${2:?usage: trial-rattan.sh <scenario> <log-file>}
LIB_DIR="$(dirname "$(realpath "$0")")"

export RATTAN_CPU="${RATTAN_WORKER_CORES:?}"
export RATTAN_WORKING_THREADS="${RATTAN_WORKER_THREADS:?}"

export CLIENT_CORES SERVER_CORES CLIENT_BARRIER SERVER_BARRIER CCA DURATION

cd "${SCENARIO_DIR:?}"

exec taskset -c "${EMULATOR_CORES:?}" "${RATTAN_BIN:-rattan}" run \
    --config "$SCENARIO.toml" \
    --right "$LIB_DIR/iperf3-server.sh" \
    --left "$LIB_DIR/iperf3-client.sh" "$LOG"
