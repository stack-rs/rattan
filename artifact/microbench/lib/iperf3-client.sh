#!/usr/bin/env bash

set -euo pipefail

LOG=${1:?usage: iperf3-client.sh <log-file>}

SERVER=${RATTAN_BASE:-}${MAHIMAHI_BASE:-}${MININET_BASE:-}
SERVER=${SERVER:-127.0.0.1}
CLIENT_CORES=${CLIENT_CORES:-0-$(($(getconf _NPROCESSORS_ONLN) - 1))}
CLIENT_BARRIER=${CLIENT_BARRIER:?CLIENT_BARRIER must be set}
CCA=${CCA:-cubic}
DURATION=${DURATION:-10}

FALLBACK_TIMEOUT=$((DURATION * 4 + 30))

seconds_left() {
    local line deadline now
    read -r line <"$CLIENT_BARRIER" || line=""
    if [[ $line =~ DEADLINE[[:space:]]+([^[:space:]]+) ]]; then
        deadline=$(date -d "${BASH_REMATCH[1]}" +%s.%N 2>/dev/null) || {
            echo "$FALLBACK_TIMEOUT"
            return
        }
        now=$(date +%s.%N)
        awk -v d="$deadline" -v n="$now" 'BEGIN { printf "%.3f\n", d - n }'
    else
        echo "$FALLBACK_TIMEOUT"
    fi
}

while [ ! -f "$CLIENT_BARRIER" ]; do
    sleep 0.05
done

budget=$(seconds_left)

{
    echo "# server=$SERVER cca=$CCA seconds=$DURATION cores=$CLIENT_CORES"
    cat "$CLIENT_BARRIER"
} >>"$LOG"

if awk -v b="$budget" 'BEGIN { exit (b >= 0.5) ? 1 : 0 }'; then
    echo "# missed the group's window, not started" >>"$LOG"
    echo "# iperf3 exit status: not started" >>"$LOG"
    exit 1
fi

set +e
timeout --kill-after=2 "$budget" \
    taskset -c "$CLIENT_CORES" \
    iperf3 --client "$SERVER" --reverse --congestion "$CCA" \
    --format k --time "$DURATION" |
    tee -a "$LOG"
status=${PIPESTATUS[0]}
set -e

echo "# iperf3 exit status: $status" >>"$LOG"
exit "$status"
