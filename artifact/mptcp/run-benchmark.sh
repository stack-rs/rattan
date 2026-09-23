#!/usr/bin/env bash

set -euo pipefail

MPTCP_DIR="$(dirname "$(realpath "$0")")"
# shellcheck source=../scripts/lib.sh
source "$MPTCP_DIR/../scripts/lib.sh"

LOG_TAG=mptcp

CONFIG_DIR="$MPTCP_DIR/config"
RESULTS_ROOT="$MPTCP_DIR/results"
APP_DIR="$MPTCP_DIR/app"

# Same size as the runs behind Table 2.
export TRANSFER_BYTES=${TRANSFER_BYTES:-53433800}

declare -A SCHEDULER_NAME=([default]=minRTT [blest]=blest [ecf]=ecf)

ALL_SCENARIOS=(a b c d)
ALL_SCHEDULERS=(default blest ecf)
ALL_CCAS=(cubic bbr vegas lia olia balia)

usage() {
    cat >&2 <<'EOF'
Measure MPTCP flow completion time on four path scenarios (§5.2, Table 2).

Usage: run-benchmark.sh [--quick | --paper] [options]

Presets:
  --quick   3 runs per combination (default)
  --paper   10 runs per combination, as in the paper

Options:
  --repeat N            override the number of runs per combination
  --scenarios a,b,c,d   which scenarios to run
  --schedulers a,b,c    which MPTCP schedulers (default,blest,ecf)
  --ccas a,b,c          which congestion control algorithms
  --workers N           how many runs to have in flight at once
                        (default: cores / 2, which is one per pinned pair)
  -h, --help            show this
EOF
    exit 1
}

preset=quick
repeat=""
scenarios=""
schedulers=""
ccas=""
workers=""

while [ $# -gt 0 ]; do
    case $1 in
    --quick) preset=quick ;;
    --paper) preset=paper ;;
    --repeat)
        repeat=${2:?--repeat needs a number}
        shift
        ;;
    --scenarios)
        scenarios=${2:?--scenarios needs a list}
        shift
        ;;
    --schedulers)
        schedulers=${2:?--schedulers needs a list}
        shift
        ;;
    --ccas)
        ccas=${2:?--ccas needs a list}
        shift
        ;;
    --workers)
        workers=${2:?--workers needs a number}
        shift
        ;;
    -h | --help) usage ;;
    *) die "unknown argument '$1'. Try --help." ;;
    esac
    shift
done

case $preset in
quick) : "${repeat:=3}" ;;
paper) : "${repeat:=10}" ;;
esac

read_list() {
    local given=$1
    shift
    if [ -z "$given" ]; then
        echo "$@"
    else
        echo "${given//,/ }"
    fi
}

read -r -a scenario_list <<<"$(read_list "$scenarios" "${ALL_SCENARIOS[@]}")"
read -r -a scheduler_list <<<"$(read_list "$schedulers" "${ALL_SCHEDULERS[@]}")"
read -r -a cca_list <<<"$(read_list "$ccas" "${ALL_CCAS[@]}")"

for scenario in "${scenario_list[@]}"; do
    [ -f "$CONFIG_DIR/scenario-$scenario.toml" ] ||
        die "no such scenario '$scenario'; expected one of ${ALL_SCENARIOS[*]}"
done
for scheduler in "${scheduler_list[@]}"; do
    [ -n "${SCHEDULER_NAME[$scheduler]:-}" ] ||
        die "unknown scheduler '$scheduler'; expected one of ${ALL_SCHEDULERS[*]}"
done

require_not_root
require_cmd rattan cargo taskset iptables

[ -d /proc/sys/net/mptcp ] ||
    die "this kernel has no MPTCP v0.96. Boot the kernel setup-vm.sh built:
  uname -r should report 5.4.301, and /proc/sys/net/mptcp should exist."

log "Kernel $(uname -r), MPTCP protocol version $(cat /proc/sys/net/mptcp/mptcp_version)"

keep_sudo_alive

sudo sysctl -q -w net.mptcp.mptcp_enabled=1

log "Loading congestion control modules"
for cca in "${cca_list[@]}"; do
    sudo modprobe "tcp_$cca" 2>/dev/null || true
done
available=$(cat /proc/sys/net/ipv4/tcp_available_congestion_control)
for cca in "${cca_list[@]}"; do
    [[ " $available " == *" $cca "* ]] ||
        die "this kernel cannot use '$cca'. Available: $available"
done

log "Building the transfer application"
(cd "$APP_DIR" && cargo build --release --quiet)
export APP="$APP_DIR/target/release/mptcp_app"
[ -x "$APP" ] || die "the build produced no $APP"

log "Generating the bandwidth traces scenarios C and D replay"
python3 "$MPTCP_DIR/lib/gen-traces.py"

cores=$(cpu_count)
: "${workers:=$((cores / 2))}"
[ "$workers" -ge 1 ] || die "need at least 2 cores"
if [ "$((workers * 2))" -gt "$cores" ]; then
    die "--workers $workers needs $((workers * 2)) cores, this machine has $cores"
fi

worker_cores() { echo "$1,$(($1 + workers))"; }

run_dir="$RESULTS_ROOT/mptcp-$preset-$(date -u +%y%m%dT%H%M%SZ)"
mkdir -p "$run_dir"

{
    echo "# Run metadata, written by $(basename "$0")"
    echo "experiment = \"mptcp\""
    echo "preset = \"$preset\""
    echo "started_utc = \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\""
    echo "kernel = \"$(uname -r)\""
    echo "mptcp_protocol_version = $(cat /proc/sys/net/mptcp/mptcp_version)"
    echo "rattan_version = \"$(rattan --version 2>/dev/null | tr '\n' ' ')\""
    echo "transfer_bytes = $TRANSFER_BYTES"
    echo "runs_per_combination = $repeat"
    echo "cpus = $cores"
    echo "workers = $workers"
    echo "scenarios = $(toml_strings "${scenario_list[@]}")"
    echo "schedulers = $(toml_strings "${scheduler_list[@]}")"
    echo "congestion_control = $(toml_strings "${cca_list[@]}")"
} >"$run_dir/run.toml"

log "Results will be in $run_dir"
log "$workers runs at a time, two cores each"

log "Checking that a transfer uses both paths"
sudo sysctl -q -w net.mptcp.mptcp_path_manager=fullmesh
preflight="$run_dir/.preflight"
"$MPTCP_DIR/lib/run-one.sh" \
    "$CONFIG_DIR/scenario-${scenario_list[0]}.toml" "$preflight" "$(worker_cores 0)" ||
    die "the first run failed. See $preflight/rattan.log and $preflight/sender.log."

paths=$(grep -m1 '^PATHS ' "$preflight/sender.log" || true)
[ -n "$paths" ] ||
    die "the first run printed no PATHS line; see $preflight/sender.log"
tx1=${paths#*tx1=}
tx1=${tx1%% *}
tx2=${paths##*tx2=}

least=$((TRANSFER_BYTES / 20))
if [ "$tx1" -lt "$least" ] || [ "$tx2" -lt "$least" ]; then
    die "only one path carried the transfer: $tx1 and $tx2 bytes sent.
  MPTCP is not opening the second subflow. Check that
  net.mptcp.mptcp_enabled is 1 and that this is the 5.4.301 kernel
  setup-vm.sh built, not a kernel with the in-tree MPTCP."
fi
log "  both paths used: $tx1 and $tx2 bytes"
rm -rf "$preflight"

claim_dir="$run_dir/.claimed"

run_group() {
    local scheduler=$1 cca=$2
    local tasks=() scenario index worker pids=()

    sudo sysctl -q -w net.mptcp.mptcp_path_manager=fullmesh
    sudo sysctl -q -w net.mptcp.mptcp_scheduler="$scheduler"
    sudo sysctl -q -w net.ipv4.tcp_congestion_control="$cca"

    local got_scheduler got_cca
    got_scheduler=$(cat /proc/sys/net/mptcp/mptcp_scheduler)
    got_cca=$(cat /proc/sys/net/ipv4/tcp_congestion_control)
    [ "$got_scheduler" = "$scheduler" ] ||
        die "asked for scheduler '$scheduler', kernel reports '$got_scheduler'"
    [ "$got_cca" = "$cca" ] ||
        die "asked for '$cca', kernel reports '$got_cca'"

    for scenario in "${scenario_list[@]}"; do
        for ((index = 1; index <= repeat; index++)); do
            tasks+=("$scenario:$index")
        done
    done

    rm -rf "$claim_dir"
    mkdir -p "$claim_dir"

    for ((worker = 0; worker < workers; worker++)); do
        (
            local task k=0
            for task in "${tasks[@]}"; do
                k=$((k + 1))
                mkdir "$claim_dir/$k" 2>/dev/null || continue
                local scenario=${task%%:*} index=${task##*:}
                local dir="$run_dir/$scenario/${SCHEDULER_NAME[$scheduler]}_$cca/run$index"
                "$MPTCP_DIR/lib/run-one.sh" \
                    "$CONFIG_DIR/scenario-$scenario.toml" \
                    "$dir" "$(worker_cores "$worker")" || true
            done
        ) &
        pids+=("$!")
        sleep 0.7
    done

    for worker in "${pids[@]}"; do
        wait "$worker"
    done
    rm -rf "$claim_dir"
}

total_groups=$((${#scheduler_list[@]} * ${#cca_list[@]}))
group=0
for scheduler in "${scheduler_list[@]}"; do
    for cca in "${cca_list[@]}"; do
        group=$((group + 1))
        printf '[%s] group %2d/%-2d  %-7s %-6s  %d runs\n' \
            "$LOG_TAG" "$group" "$total_groups" \
            "${SCHEDULER_NAME[$scheduler]}" "$cca" \
            "$((${#scenario_list[@]} * repeat))"
        run_group "$scheduler" "$cca"
    done
done

"$MPTCP_DIR/../analyze/report.sh" mptcp "$run_dir"
