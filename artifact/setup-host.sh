#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=scripts/lib.sh
source scripts/lib.sh

VAGRANT_LIBVIRT_INSTALLER='https://raw.githubusercontent.com/vagrant-libvirt/vagrant-libvirt-qa/main/scripts/install.bash'

require_not_root

if [ ! -e /dev/kvm ]; then
    die "/dev/kvm is missing. Enable hardware virtualization (Intel VT-x or AMD-V) in the firmware."
fi

log "Installing libvirt, QEMU and Vagrant"
if have_cmd vagrant && vagrant plugin list 2>/dev/null | grep -q vagrant-libvirt; then
    log "  already present, skipping"
else
    installer=$(mktemp)
    trap 'rm -f "$installer"' EXIT
    curl -fsSL "$VAGRANT_LIBVIRT_INSTALLER" -o "$installer"
    bash "$installer"
fi

log "Installing rsync, used to copy this repository into the guests"
apt_install rsync

log "Adding $USER to the kvm and libvirt groups"
sudo usermod -aG kvm "$USER"
sudo usermod -aG libvirt "$USER"

if have_cmd ufw && sudo ufw status 2>/dev/null | grep -q '^Status: active'; then
    log "Allowing traffic from the guest network 192.168.121.0/24"
    sudo ufw allow from 192.168.121.0/24
else
    log "No active ufw found. If you run another firewall, allow traffic from 192.168.121.0/24"
fi

cat <<'EOF'

Host setup done.

Log out and back in for the group membership to take effect, then:

    vagrant up micro      # microbenchmarks (§5.1), ~25 min
    vagrant up mptcp      # multipath benchmark (§5.2), ~20 min + kernel build

EOF
