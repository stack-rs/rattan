#!/usr/bin/env bash

set -euo pipefail

ANALYZE_DIR="$(dirname "$(realpath "$0")")"
# shellcheck source=../scripts/lib.sh
source "$ANALYZE_DIR/../scripts/lib.sh"

EXPERIMENT=${1:?usage: report.sh <experiment> <run-directory>}
RUN_DIR=${2:?usage: report.sh <experiment> <run-directory>}

[ -d "$RUN_DIR" ] || die "no such directory: $RUN_DIR"

log "Reading the trial logs"
python3 "$ANALYZE_DIR/collect.py" "$EXPERIMENT" "$RUN_DIR"

log "Writing the CSVs"
python3 "$ANALYZE_DIR/to_csv.py" "$RUN_DIR"

if [ "$EXPERIMENT" != mptcp ]; then
    if python3 -c 'import matplotlib' 2>/dev/null; then
        log "Drawing the figures"
        python3 "$ANALYZE_DIR/plot.py" "$RUN_DIR"
    else
        warn "matplotlib is not installed, skipping the figures"
    fi
fi

echo
log "Everything above is saved in $RUN_DIR"
