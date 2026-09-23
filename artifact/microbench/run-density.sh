#!/usr/bin/env bash

set -euo pipefail

MICROBENCH_DIR="$(dirname "$(realpath "$0")")"
# shellcheck source=lib/common.sh
source "$MICROBENCH_DIR/lib/common.sh"

LOG_TAG=density

EMULATOR_CORE_LIST=(0 1 2 3 4 5 6 7)
export CLIENT_CORES="12-15"
export SERVER_CORES="12-15"

export RATTAN_BIN=rattan
export RATTAN_WORKER_THREADS=1

BANDWIDTH_MBPS=16
DELAY_MS=20

usage() {
    cat >&2 <<'EOF'
Measure how many paths one machine can emulate at once (§5.1, Figure 7).

Usage: run-density.sh [--quick | --paper] [options]

Presets:
  --quick   concurrency 8,32,128,256,512, 128 flows per level (default)
  --paper   concurrency 8,16,32,64,128,160,192,224,256,320,384,448,512,
            512 flows per level, as in the paper

Options:
  --levels a,b,c        override the concurrency levels
  --flows-per-level N   override how many flows each level contributes
  --modes static,trace  which of Figure 7's two panels to produce
  --emulators a,b,c     which emulators to run (default rattan,mahimahi,mininet)
  -h, --help            show this

Environment:
  START_SLACK=N   how long after a level is released an instance may still be
                  starting, in seconds (default 1). The measurement window is
                  DURATION + this.
EOF
    exit 1
}

preset=quick
levels=""
flows_per_level=""
modes="static,trace"
emulators="rattan,mahimahi,mininet"

while [ $# -gt 0 ]; do
    case $1 in
    --quick) preset=quick ;;
    --paper) preset=paper ;;
    --levels)
        levels=${2:?--levels needs a list}
        shift
        ;;
    --flows-per-level)
        flows_per_level=${2:?--flows-per-level needs a number}
        shift
        ;;
    --modes)
        modes=${2:?--modes needs a list}
        shift
        ;;
    --emulators)
        emulators=${2:?--emulators needs a list}
        shift
        ;;
    -h | --help) usage ;;
    *) die "unknown argument '$1'. Try --help." ;;
    esac
    shift
done

case $preset in
quick)
    : "${levels:=8,32,128,256,512}"
    : "${flows_per_level:=128}"
    ;;
paper)
    : "${levels:=8,16,32,64,128,160,192,224,256,320,384,448,512}"
    : "${flows_per_level:=512}"
    ;;
esac

IFS=',' read -r -a level_list <<<"$levels"
for level in "${level_list[@]}"; do
    [ "$level" -ge 1 ] 2>/dev/null || die "concurrency level '$level' is not a positive number"
done

IFS=',' read -r -a mode_list <<<"$modes"
for mode in "${mode_list[@]}"; do
    case $mode in
    static | trace) ;;
    *) die "unknown mode '$mode'; expected static or trace" ;;
    esac
done

IFS=',' read -r -a emulator_list <<<"$emulators"
for emulator in "${emulator_list[@]}"; do
    case $emulator in
    rattan | mahimahi | mininet) ;;
    *) die "unknown emulator '$emulator'" ;;
    esac
done

require_emulators "${emulator_list[@]}"
keep_sudo_alive
apply_sysctl density
reset_environment

declare -A SCENARIO_OF
for mode in "${mode_list[@]}"; do
    SCENARIO_OF[$mode]=$(python3 "$MICROBENCH_DIR/lib/gen-scenarios.py" "$SCENARIO_DIR" \
        density --mode "$mode" --bandwidth-mbps "$BANDWIDTH_MBPS" --delay-ms "$DELAY_MS")
done

emulators_for_mode() {
    local mode=$1 emulator out=()
    for emulator in "${emulator_list[@]}"; do
        if [ "$mode" = trace ] && [ "$emulator" = mininet ]; then
            continue
        fi
        out+=("$emulator")
    done
    echo "${out[@]}"
}

START_SLACK=${START_SLACK:-1}

run_dir=$(new_run_dir density "$preset")
write_run_metadata "$run_dir" \
    "experiment = \"density\"" \
    "preset = \"$preset\"" \
    "measurement_window_seconds = $((DURATION + START_SLACK))" \
    "bandwidth_mbps = $BANDWIDTH_MBPS" \
    "delay_ms_each_way = $DELAY_MS" \
    "concurrency_levels = [$(
        IFS=,
        echo "${level_list[*]}"
    )]" \
    "flows_per_level = $flows_per_level" \
    "modes = $(toml_strings "${mode_list[@]}")" \
    "emulators = $(toml_strings "${emulator_list[@]}")" \
    "emulator_cores = \"$(
        IFS=,
        echo "${EMULATOR_CORE_LIST[*]}"
    )\""

log "Results will be in $run_dir"

GROUP_FAILURES=0

run_group() {
    local emulator=$1 scenario=$2 label=$3 concurrency=$4
    local group_dir client_barrier j core log started deadline release pids=() failures=0

    reapply_sysctl
    group_dir="$run_dir/.sync/$(new_trial_id)"
    mkdir -p "$group_dir"
    client_barrier="$group_dir/client.sync"

    for ((j = 1; j <= concurrency; j++)); do
        core=${EMULATOR_CORE_LIST[(j - 1) % ${#EMULATOR_CORE_LIST[@]}]}
        log=$(trial_log_path "$run_dir" "$emulator" "$label")
        EMULATOR_CORES="$core" \
            RATTAN_WORKER_CORES="$core" \
            MININET_ISOLATE=1 \
            SERVER_BARRIER="$group_dir/$j.start" \
            CLIENT_BARRIER="$client_barrier" \
            "$MICROBENCH_DIR/lib/trial-$emulator.sh" "$scenario" "$log" \
            >"$(emulator_log_path "$log")" 2>&1 &
        pids+=("$!")
    done

    deadline=$(($(date +%s) + 120 + concurrency))
    while :; do
        started=$(find "$group_dir" -maxdepth 1 -name '*.start' | wc -l)
        [ "$started" -ge "$concurrency" ] && break
        if [ "$(date +%s)" -ge "$deadline" ]; then
            warn "$emulator c$concurrency: only $started of $concurrency servers came up"
            break
        fi
        sleep 0.1
    done

    release=$(date -u +%Y-%m-%dT%H:%M:%S.%NZ)
    printf 'SYNC %s DEADLINE %s\n' \
        "$release" \
        "$(date -u -d "$release + $((DURATION + START_SLACK)) seconds" +%Y-%m-%dT%H:%M:%S.%NZ)" \
        >"$group_dir/sync.pending"
    mv "$group_dir/sync.pending" "$client_barrier"

    for j in "${pids[@]}"; do
        wait "$j" || failures=$((failures + 1))
    done
    rm -rf "$group_dir"

    GROUP_FAILURES=$failures
}

for mode in "${mode_list[@]}"; do
    scenario=${SCENARIO_OF[$mode]}
    read -r -a mode_emulators <<<"$(emulators_for_mode "$mode")"

    for level in "${level_list[@]}"; do
        groups=$(((flows_per_level + level - 1) / level))
        for emulator in "${mode_emulators[@]}"; do
            label="$scenario-c$level"
            failed=0
            for ((group = 1; group <= groups; group++)); do
                printf '[%s] %-6s %-9s c%-4s group %3d/%-3d\n' \
                    "$LOG_TAG" "$mode" "$emulator" "$level" "$group" "$groups"
                run_group "$emulator" "$scenario" "$label" "$level"
                failed=$((failed + GROUP_FAILURES))
                sleep 1
            done
            if [ "$failed" -gt 0 ]; then
                warn "$emulator $mode c$level: $failed of $((groups * level)) flows failed"
            fi
        done
        reset_environment
    done
done

reset_environment

"$MICROBENCH_DIR/../analyze/report.sh" density "$run_dir"
