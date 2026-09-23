#!/usr/bin/env bash

set -euo pipefail

LIB_DIR="$(dirname "$(realpath "$0")")"
cd "${SCENARIO_DIR:?}"

read -r -a MAHIMAHI_CMD <<<"${MAHIMAHI_CMD:?}"

"$LIB_DIR/iperf3-server.sh" &
server=$!

set +e
taskset -c "${EMULATOR_CORES:?}" \
    "${MAHIMAHI_CMD[@]}" \
    "$LIB_DIR/iperf3-client.sh" "${LOG:?}"
status=$?
set -e

wait "$server" || true
exit "$status"
