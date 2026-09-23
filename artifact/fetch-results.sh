#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=scripts/lib.sh
source scripts/lib.sh

: "${LOG_TAG:=fetch}"

case ${1:-} in
micro) source_dir=rattan/artifact/microbench/results ;;
mptcp) source_dir=rattan/artifact/mptcp/results ;;
-h | --help)
    echo "usage: fetch-results.sh <micro|mptcp>"
    exit 0
    ;;
*) die "usage: fetch-results.sh <micro|mptcp>" ;;
esac
guest=$1

require_cmd vagrant rsync ssh

ssh_config=$(mktemp)
trap 'rm -f "$ssh_config"' EXIT
vagrant ssh-config "$guest" >"$ssh_config" 2>/dev/null ||
    die "cannot reach the $guest guest. Start it with: vagrant up $guest"

dest="results/$guest"
mkdir -p "$dest"

log "Copying $source_dir/ from $guest to $dest/"
rsync -a -e "ssh -F $ssh_config" "$guest:$source_dir/" "$dest/" ||
    die "the copy failed. Has an experiment finished in $guest yet?"

in_guest=$(ssh -F "$ssh_config" "$guest" "find $source_dir -type f | wc -l")
on_host=$(find "$dest" -type f | wc -l)
[ "$on_host" -ge "$in_guest" ] ||
    die "the guest has $in_guest files, only $on_host arrived"
log "Done: $in_guest files, runs in $dest/:"
find "$dest" -mindepth 1 -maxdepth 1 -type d -printf '  %f\n' | sort
