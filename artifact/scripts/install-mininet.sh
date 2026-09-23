#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=lib.sh
source lib.sh
# shellcheck source=versions.sh
source versions.sh

: "${LOG_TAG:=install:mininet}"
PATCH="$(realpath ../patches/mininet-raise-bwParamMax.patch)"

if python3 -c 'import mininet' 2>/dev/null && have_cmd mnexec; then
    log "  already installed"
    exit 0
fi

log "  installing dependencies"
apt_install \
    openvswitch-switch openvswitch-common \
    python3-setuptools python3-dev help2man \
    socat psmisc iputils-ping

mkdir -p "$TOOLS_DIR"
clone_at "$MININET_REPO" "$MININET_COMMIT" "$TOOLS_DIR/mininet"

cd "$TOOLS_DIR/mininet"
log "  applying the bandwidth-ceiling patch"
if git apply --check "$PATCH" 2>/dev/null; then
    git apply "$PATCH"
elif git apply --reverse --check "$PATCH" 2>/dev/null; then
    log "    already applied"
else
    die "the patch does not apply to mininet $MININET_COMMIT"
fi
grep -q 'bwParamMax = 100000' mininet/link.py || die "patch applied but bwParamMax is not 100000"

log "  building and installing"
sudo PIP_BREAK_SYSTEM_PACKAGES=1 make install PYTHON=python3

sudo systemctl enable --now openvswitch-switch
sudo mn -c >/dev/null 2>&1 || true

log "  installed mininet $(python3 -c 'import mininet.net; print(mininet.net.VERSION)')"
