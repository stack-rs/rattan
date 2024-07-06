#!/usr/bin/env bash

PING_ITERS=10
WORKDIR=$(
	cd "$(dirname "$0")"
	pwd
)

echo "CLI Test #1"
RUST_LOG=info \
    cargo run --release -q -p rattan -- \
    --uplink-delay 10ms \
    --downlink-delay 20ms \
    --uplink-loss 0.01 \
    --downlink-loss 0.02 \
    --uplink-bandwidth 10Mbps \
    --uplink-queue droptail \
    --uplink-queue-args "{\"packet_limit\":100}" \
    --downlink-bandwidth 20Mbps \
    --downlink-queue=codel \
    --downlink-queue-args="{\"packet_limit\":100,\"target\":\"50ms\"}" \
    -- $WORKDIR/ping.sh $PING_ITERS '$RATTAN_BASE'
