#!/usr/bin/env bash

set -uo pipefail

LOG_DIR=${1:?usage: receiver.sh <log-directory>}
exec >"$LOG_DIR/receiver.log" 2>&1

ID=${RATTAN_ID:?}
IF1="vR1-R-$ID"
IF2="vR2-R-$ID"

echo "started $(date -u +%Y-%m-%dT%H:%M:%SZ)"
ip -o -4 addr show dev "$IF1"
ip -o -4 addr show dev "$IF2"

ip route replace "${RATTAN_IP_1:?}/32" dev "$IF1"
ip route replace "${RATTAN_IP_2:?}/32" dev "$IF2"

"${APP:?}" --server --address 0.0.0.0 --file "$LOG_DIR/ready.lock"
status=$?

ip -s link show "$IF1"
ip -s link show "$IF2"

echo "finished $(date -u +%Y-%m-%dT%H:%M:%SZ) status=$status"
exit "$status"
