#!/usr/bin/env bash

set -euo pipefail

MICROBENCH_DIR="$(dirname "$(realpath "$0")")"
# shellcheck source=lib/common.sh
source "$MICROBENCH_DIR/lib/common.sh"

LOG_TAG=throughput

export RATTAN_WORKER_CORES="4,5,6,7"
export RATTAN_WORKER_THREADS=4
export EMULATOR_CORES="0,2,4,5,6,7"
export CLIENT_CORES="12-15"
export SERVER_CORES="12-15"

export RATTAN_BIN=rattan-rv

DELAY_MS=25

usage() {
    cat >&2 <<'EOF'
Measure single-channel forwarding throughput (§5.1, Figure 5).

Usage: run-throughput.sh [--quick | --paper] [options]

Presets:
  --quick   0.5 to 6 Gbps in 0.5 Gbps steps, 3 trials each (default)
  --paper   0.1 to 6 Gbps in 0.1 Gbps steps, 5 trials each, as in the paper

Options:
  --trials N            override the number of trials per bandwidth
  --max-gbps N          override the top of the sweep
  --emulators a,b,c     which emulators to run (default rattan,mahimahi,mininet)
  -h, --help            show this
EOF
    exit 1
}

preset=quick
trials=""
max_gbps=6
emulators="rattan,mahimahi,mininet"

while [ $# -gt 0 ]; do
    case $1 in
    --quick) preset=quick ;;
    --paper) preset=paper ;;
    --trials)
        trials=${2:?--trials needs a number}
        shift
        ;;
    --max-gbps)
        max_gbps=${2:?--max-gbps needs a number}
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
    step_mbps=500
    : "${trials:=3}"
    ;;
paper)
    step_mbps=100
    : "${trials:=5}"
    ;;
esac

IFS=',' read -r -a emulator_list <<<"$emulators"
for emulator in "${emulator_list[@]}"; do
    case $emulator in
    rattan | mahimahi | mininet) ;;
    *) die "unknown emulator '$emulator'" ;;
    esac
done

max_mbps=$(awk -v g="$max_gbps" 'BEGIN { printf "%d", g * 1000 }')

require_emulators "${emulator_list[@]}"
keep_sudo_alive
[[ " ${emulator_list[*]} " == *" rattan "* ]] && ensure_rvnic_loaded
apply_sysctl throughput
reset_environment

bandwidths=()
for ((mbps = step_mbps; mbps <= max_mbps; mbps += step_mbps)); do
    bandwidths+=("$mbps")
done

log "Generating ${#bandwidths[@]} scenarios"
scenarios=()
for mbps in "${bandwidths[@]}"; do
    scenarios+=("$(python3 "$MICROBENCH_DIR/lib/gen-scenarios.py" "$SCENARIO_DIR" \
        throughput --bandwidth-mbps "$mbps" --delay-ms "$DELAY_MS")")
done

run_dir=$(new_run_dir throughput "$preset")
write_run_metadata "$run_dir" \
    "experiment = \"throughput\"" \
    "preset = \"$preset\"" \
    "delay_ms_each_way = $DELAY_MS" \
    "bandwidths_mbps = [$(
        IFS=,
        echo "${bandwidths[*]}"
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

"$MICROBENCH_DIR/../analyze/report.sh" throughput "$run_dir"
