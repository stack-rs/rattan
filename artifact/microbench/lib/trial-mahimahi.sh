#!/usr/bin/env bash

set -euo pipefail

SCENARIO=${1:?usage: trial-mahimahi.sh <scenario> <log-file>}
LOG=${2:?usage: trial-mahimahi.sh <scenario> <log-file>}
LIB_DIR="$(dirname "$(realpath "$0")")"

cd "${SCENARIO_DIR:?}"

read -r -a MAHIMAHI_CMD <"$SCENARIO.mm"

exec "$LIB_DIR/netns-wrapper.sh" \
    env \
    "SCENARIO_DIR=$SCENARIO_DIR" \
    "CLIENT_CORES=${CLIENT_CORES:?}" \
    "SERVER_CORES=${SERVER_CORES:?}" \
    "CLIENT_BARRIER=${CLIENT_BARRIER:?}" \
    "SERVER_BARRIER=${SERVER_BARRIER:?}" \
    "CCA=${CCA:?}" \
    "DURATION=${DURATION:?}" \
    "EMULATOR_CORES=${EMULATOR_CORES:?}" \
    "MAHIMAHI_CMD=${MAHIMAHI_CMD[*]}" \
    "LOG=$LOG" \
    "$LIB_DIR/mahimahi-inner.sh"
