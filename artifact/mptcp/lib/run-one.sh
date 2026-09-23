#!/usr/bin/env bash

set -uo pipefail

CONFIG=${1:?usage: run-one.sh <scenario.toml> <log-directory> <cores>}
LOG_DIR=${2:?usage: run-one.sh <scenario.toml> <log-directory> <cores>}
CORES=${3:?usage: run-one.sh <scenario.toml> <log-directory> <cores>}
LIB_DIR="$(dirname "$(realpath "$0")")"

mkdir -p "$LOG_DIR"

cd "$(dirname "$(realpath "$CONFIG")")" || exit 1

# shellcheck disable=SC2024
sudo taskset -c "$CORES" \
    env "APP=${APP:?}" "TRANSFER_BYTES=${TRANSFER_BYTES:?}" \
    rattan run --config "$(basename "$CONFIG")" \
    --left "$LIB_DIR/sender.sh" --left "$LOG_DIR" \
    --right "$LIB_DIR/receiver.sh" --right "$LOG_DIR" \
    >"$LOG_DIR/rattan.log" 2>&1
status=$?

rm -f "$LOG_DIR/ready.lock"

if [ "$status" -ne 0 ]; then
    echo "rattan exited $status, see $LOG_DIR/rattan.log" >&2
fi
exit "$status"
