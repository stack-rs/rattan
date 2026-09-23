# shellcheck shell=bash

# shellcheck source=../../scripts/lib.sh
source "$MICROBENCH_DIR/../scripts/lib.sh"

export SCENARIO_DIR="$MICROBENCH_DIR/scenarios"
RESULTS_ROOT="$MICROBENCH_DIR/results"

export DURATION=${DURATION:-10}
export CCA=${CCA:-cubic}

require_emulators() {
    require_cmd iperf3 taskset
    for emulator in "$@"; do
        case $emulator in
        rattan) require_cmd rattan rattan-rv ;;
        mahimahi) require_cmd mm-link mm-delay mm-loss ;;
        mininet) require_cmd mn ovs-vsctl ;;
        esac
    done
}

apply_sysctl() {
    local profile="$MICROBENCH_DIR/sysctl/$1.conf"
    [ -f "$profile" ] || die "no sysctl profile $1"
    SYSCTL_PROFILE=$profile
    sudo sysctl -q -p "$profile"
}

reapply_sysctl() { sudo sysctl -q -p "${SYSCTL_PROFILE:?}"; }

ensure_rvnic_loaded() {
    if lsmod | grep -q '^rattan_vnic'; then
        return
    fi
    sudo modprobe rattan_vnic num_queues=1 ||
        die "cannot load the rattan_vnic module. Re-run 'setup-vm.sh micro'."
}

reset_environment() {
    sudo pkill -f 'iperf3 -s' 2>/dev/null || true
    sudo mn -c >/dev/null 2>&1 || true
    sudo "$MICROBENCH_DIR/../../scripts/clean_stdenv.sh" --all >/dev/null 2>&1 || true
}

new_run_dir() {
    local experiment=$1 preset=$2
    local dir
    dir="$RESULTS_ROOT/$experiment-$preset-$(date -u +%y%m%dT%H%M%SZ)"
    mkdir -p "$dir"
    echo "$dir"
}

new_trial_id() {
    printf '%s-%s\n' "$(date -u +%H%M%S)" "$(head -c 3 /dev/urandom | od -An -tx1 | tr -d ' \n')"
}

trial_log_path() {
    local run_dir=$1 emulator=$2 scenario=$3
    local dir="$run_dir/$emulator/$scenario"
    mkdir -p "$dir"
    echo "$dir/$(new_trial_id).log"
}

emulator_log_path() { echo "${1%.log}.emulator.txt"; }

run_one_trial() {
    local emulator=$1 scenario=$2 run_dir=$3
    local attempts=${TRIAL_ATTEMPTS:-3} attempt log barrier

    for ((attempt = 1; attempt <= attempts; attempt++)); do
        reapply_sysctl
        log=$(trial_log_path "$run_dir" "$emulator" "$scenario")
        barrier="$run_dir/.barrier.$$"
        rm -f "$barrier"

        if CLIENT_BARRIER="$barrier" SERVER_BARRIER="$barrier" \
            "$MICROBENCH_DIR/lib/trial-$emulator.sh" "$scenario" "$log" \
            >"$(emulator_log_path "$log")" 2>&1; then
            rm -f "$barrier"
            return 0
        fi

        rm -f "$barrier"
        warn "$emulator/$scenario attempt $attempt of $attempts failed"
        reset_environment
        sleep 2
    done

    warn "$emulator/$scenario: giving up, see $log"
    return 1
}

write_run_metadata() {
    local run_dir=$1
    shift
    {
        echo "# Run metadata, written by $(basename "$0")"
        echo "started_utc = \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\""
        echo "kernel = \"$(uname -r)\""
        echo "cpus = $(cpu_count)"
        echo "rattan_version = \"$(rattan --version 2>/dev/null | tr '\n' ' ')\""
        echo "congestion_control = \"$CCA\""
        echo "iperf3_seconds = $DURATION"
        echo "client_cores = \"$CLIENT_CORES\""
        echo "server_cores = \"$SERVER_CORES\""
        for entry in "$@"; do
            echo "$entry"
        done
    } >"$run_dir/run.toml"
}
