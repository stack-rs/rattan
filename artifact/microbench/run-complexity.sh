#!/usr/bin/env bash

set -euo pipefail

MICROBENCH_DIR="$(dirname "$(realpath "$0")")"
# shellcheck source=lib/common.sh
source "$MICROBENCH_DIR/lib/common.sh"

LOG_TAG=complexity

export RATTAN_WORKER_CORES="0,1,2,3"
export RATTAN_WORKER_THREADS=4
export EMULATOR_CORES="0,1,2,3"
export CLIENT_CORES="4"
export SERVER_CORES="6"

export RATTAN_BIN=rattan-rv

BANDWIDTH_MBPS=1000
DELAY_MS=25

usage() {
    cat >&2 <<'EOF'
Measure throughput against path complexity (§5.1, Figure 6).

Usage: run-complexity.sh [--quick | --paper] [options]

Presets:
  --quick   depths 1,2,4,8,12,16,20,24, 3 trials each (default)
  --paper   depths 1 through 24, 6 trials each, as in the paper

Options:
  --trials N            override the number of trials per depth
  --max-depth N         override the deepest path (default 24)
  --emulators a,b,c     which emulators to run (default rattan,mahimahi,mininet)
  -h, --help            show this
EOF
    exit 1
}

preset=quick
trials=""
max_depth=24
emulators="rattan,mahimahi,mininet"

while [ $# -gt 0 ]; do
    case $1 in
    --quick) preset=quick ;;
    --paper) preset=paper ;;
    --trials)
        trials=${2:?--trials needs a number}
        shift
        ;;
    --max-depth)
        max_depth=${2:?--max-depth needs a number}
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

[ "$max_depth" -ge 1 ] || die "--max-depth must be at least 1"

depths=()
case $preset in
quick)
    : "${trials:=3}"
    for depth in 1 2 4 8 12 16 20 24; do
        [ "$depth" -le "$max_depth" ] && depths+=("$depth")
    done
    ;;
paper)
    : "${trials:=6}"
    for ((depth = 1; depth <= max_depth; depth++)); do
        depths+=("$depth")
    done
    ;;
esac

IFS=',' read -r -a emulator_list <<<"$emulators"
for emulator in "${emulator_list[@]}"; do
    case $emulator in
    rattan | mahimahi | mininet) ;;
    *) die "unknown emulator '$emulator'" ;;
    esac
done

require_emulators "${emulator_list[@]}"
keep_sudo_alive
[[ " ${emulator_list[*]} " == *" rattan "* ]] && ensure_rvnic_loaded
apply_sysctl complexity
reset_environment

log "Generating ${#depths[@]} scenarios"
scenarios=()
for depth in "${depths[@]}"; do
    scenarios+=("$(python3 "$MICROBENCH_DIR/lib/gen-scenarios.py" "$SCENARIO_DIR" \
        complexity --depth "$depth" --delay-ms "$DELAY_MS" \
        --bandwidth-mbps "$BANDWIDTH_MBPS")")
done

run_dir=$(new_run_dir complexity "$preset")
write_run_metadata "$run_dir" \
    "experiment = \"complexity\"" \
    "preset = \"$preset\"" \
    "bandwidth_mbps = $BANDWIDTH_MBPS" \
    "base_delay_ms_each_way = $DELAY_MS" \
    "depths = [$(
        IFS=,
        echo "${depths[*]}"
    )]" \
    "trials_per_point = $trials" \
    "emulators = $(toml_strings "${emulator_list[@]}")" \
    "emulator_cores = \"$EMULATOR_CORES\"" \
    "rattan_worker_cores = \"$RATTAN_WORKER_CORES\""

log "Results will be in $run_dir"

total=$((${#scenarios[@]} * trials * ${#emulator_list[@]}))
done_count=0
failed=0

for scenario in "${scenarios[@]}"; do
    for ((trial = 1; trial <= trials; trial++)); do
        for emulator in "${emulator_list[@]}"; do
            done_count=$((done_count + 1))
            printf '[%s] %4d/%-4d %-9s %s\n' \
                "$LOG_TAG" "$done_count" "$total" "$emulator" "$scenario"
            run_one_trial "$emulator" "$scenario" "$run_dir" || failed=$((failed + 1))
        done
    done
done

reset_environment
log "$((total - failed))/$total trials produced a log"

"$MICROBENCH_DIR/../analyze/report.sh" complexity "$run_dir"
