#!/usr/bin/env bash

set -uo pipefail

LOG_DIR=${1:?usage: sender.sh <log-directory>}
exec >"$LOG_DIR/sender.log" 2>&1

ID=${RATTAN_ID:?}
IF1="vL1-L-$ID"
IF2="vL2-L-$ID"

echo "started $(date -u +%Y-%m-%dT%H:%M:%SZ)"
ip -o -4 addr show dev "$IF1"
ip -o -4 addr show dev "$IF2"

MY_IP1=$(ip -o -4 addr show dev "$IF1" | awk '{print $4}' | cut -d/ -f1)
MY_IP2=$(ip -o -4 addr show dev "$IF2" | awk '{print $4}' | cut -d/ -f1)
PEER_IP1=${RATTAN_IP_1:?}
PEER_IP2=${RATTAN_IP_2:?}

ip route replace "$PEER_IP1/32" dev "$IF1"
ip route replace "$PEER_IP2/32" dev "$IF2"

# Keep MPTCP from opening the two cross-path subflows.
iptables -A OUTPUT -s "$MY_IP2" -d "$PEER_IP1" -j REJECT
iptables -A OUTPUT -s "$MY_IP1" -d "$PEER_IP2" -j REJECT

# Without these pings, MPTCP v0.96 stays on one path.
ping -c 2 -W 1 "$PEER_IP1" >/dev/null &
ping -c 2 -W 1 "$PEER_IP2" >/dev/null &
wait

"${APP:?}" --client \
    --bytes "${TRANSFER_BYTES:?}" \
    --address "$PEER_IP1" \
    --file "$LOG_DIR/ready.lock"
status=$?

ip -s link show "$IF1"
ip -s link show "$IF2"

tx_bytes() {
    awk -F: -v want="$1" '$1 ~ "^ *"want"$" { split($2, f, " "); print f[9] }' \
        /proc/net/dev
}
echo "PATHS tx1=$(tx_bytes "$IF1") tx2=$(tx_bytes "$IF2")"

echo "finished $(date -u +%Y-%m-%dT%H:%M:%SZ) status=$status"
exit "$status"
