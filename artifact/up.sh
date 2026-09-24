#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=scripts/lib.sh
source scripts/lib.sh

: "${LOG_TAG:=up}"

case ${1:-} in
micro)
    machine=micro
    first=base
    second=build
    ;;
mptcp)
    machine=mptcp
    first=kernel
    second="expr"
    ;;
*)
    die "usage: up.sh <micro|mptcp>"
    ;;
esac

require_cmd vagrant
check_dns_conflict

log "[1/3] Creating $machine and running the '$first' stage"
vagrant up "$machine" --no-provision
vagrant rsync "$machine"
vagrant provision "$machine" --provision-with "$first"

log "[2/3] Rebooting $machine into the kernel that stage installed"
vagrant reload "$machine"

log "[3/3] Running the '$second' stage"
vagrant provision "$machine" --provision-with "$second"

log "Done. Connect with: vagrant ssh $machine"
