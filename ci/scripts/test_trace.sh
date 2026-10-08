#!/usr/bin/env bash
# Replay real cellular traces on the downlink and check that iperf3 gets
# close to the capacity each trace offers. Fails if any trace panics or the
# utilization is out of range.
set -e

DURATION=${DURATION:-20}
DELAY=${DELAY:-10ms}
MIN_UTIL=${MIN_UTIL:-0.85}
MAX_UTIL=${MAX_UTIL:-1.02}

# Traces from the Orca paper (SIGCOMM '20), pinned to a commit
NYC_COMMIT=ac6717b5f113bf899344e91ffc57e472daf29954
NYC_URL=https://raw.githubusercontent.com/Soheil-ab/Cellular-Traces-NYC/$NYC_COMMIT
NYC_TRACES=(
    "trace-2767958-taxi1 07e5e74d1b480ec193d981b278f3e13751ceaea5d84f858ace1b19d6a4e13108"
    "trace-2925703-home1 dc66eddd1e570ae52e8795d7f8f0367ebf29cf64e6504d0442facca8f05cc138"
    "trace-3093662-home3 be5064bb7496d2159d8bb1f7862063cf655e6eb473047b4f62f0891cc5b3738c"
    "trace-3109898-bus 7252474afe1b0551d0a36f6772d4eeaa183801b8f8eeaae02ac41153a3445d67"
    "trace-3189663-timessquare 604ebe5756ed24e5f8bbaed55979c7731a926c76f2821e7b6a4ebc9647e091b6"
    "trace-3199718-timessquare 91a22130d24b530f4a10c9491009e6e4ed2befbfe99f0070696ae2c1afc5f19e"
    "trace-3201037-timessquare 2a903bf70999c70f7416a442f6eb03e0c9463aca62df0adb47b27b78c78de76e"
    "trace-3219061-home 574d5c44bcb43a3d9a861e1f959b64da8a071cc69ab89458ce20678697fd04a7"
    "trace-3457194-timessquare 953990b14d4129fcde97d2d09e51f7562d2e37afa40bab56056e2f2810f5e3de"
    "trace-3458374-timessquare 9b7341ea305822c388563f94f8ecbde3a1c4d667f1bfeae4be144e21a547504f"
)

WORKDIR=$(
    cd "$(dirname "$0")"
    pwd
)
REPO_DIR=$(
    cd "$WORKDIR/../.."
    pwd
)
CACHE=$WORKDIR/cache/traces
mkdir -p "$CACHE"

# Download the traces (skipped if they are already cached)
traces=("$REPO_DIR/assets/24Mbps.trace")
for entry in "${NYC_TRACES[@]}"; do
    read -r name sum <<<"$entry"
    file=$CACHE/$name.trace
    if [ ! -f "$file" ] || ! echo "$sum  $file" | sha256sum -c --status; then
        echo "Downloading $name"
        curl -fsSL --retry 3 -o "$file" "$NYC_URL/$name"
        echo "$sum  $file" | sha256sum -c --quiet
    fi
    traces+=("$file")
done

# Build first so the timeout below only covers the test itself
cargo build --release -q -p rattan

failed=()
for trace in "${traces[@]}"; do
    name=$(basename "$trace" .trace)
    log=$CACHE/$name
    rm -f "$log.json"
    echo "Trace Test: $name; Delay $DELAY; ${DURATION}s"
    # New one-shot server for each trace, so a bad run can't leave it busy
    iperf3 -s -1 >"$log.server.log" 2>&1 &
    server_pid=$!
    sleep 1
    set +e
    RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' \
        timeout $((DURATION + 120)) \
        cargo run --release -q -p rattan -- \
        link \
        --uplink-delay $DELAY \
        --downlink-delay $DELAY \
        --downlink-trace "$trace" \
        -- bash -c "iperf3 -c \$RATTAN_BASE -R -t $DURATION -J > '$log.json'; sleep 1" \
        >"$log.log" 2>&1
    code=$?
    kill $server_pid 2>/dev/null
    wait $server_pid 2>/dev/null
    set -e
    if [ $code -ne 0 ] || grep -q "panicked" "$log.log"; then
        echo "FAIL: rattan exited with code $code"
        tail -n 20 "$log.log"
        failed+=("$name")
        continue
    fi
    if ! python3 "$WORKDIR/check_trace.py" "$trace" "$log.json" $MIN_UTIL $MAX_UTIL; then
        failed+=("$name")
    fi
done

if [ ${#failed[@]} -ne 0 ]; then
    echo "Failed traces: ${failed[*]}"
    exit 1
fi
echo "All ${#traces[@]} traces passed"
