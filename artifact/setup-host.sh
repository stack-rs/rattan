#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=scripts/lib.sh
source scripts/lib.sh
# shellcheck source=scripts/versions.sh
source scripts/versions.sh

VAGRANT_DEB_URL="https://releases.hashicorp.com/vagrant/$VAGRANT_VERSION/vagrant_${VAGRANT_VERSION}-1_amd64.deb"

require_not_root
have_cmd apt-get || die "This script supports Debian and Ubuntu hosts only."

if [ ! -e /dev/kvm ]; then
    die "/dev/kvm is missing. Enable hardware virtualization (Intel VT-x or AMD-V) in the firmware."
fi

log "Installing libvirt, QEMU and rsync"
apt_install libvirt-daemon-system libvirt-clients qemu-system-x86 qemu-utils \
    dnsmasq-base ebtables rsync wget \
    libvirt-dev pkg-config gcc make # to build the vagrant-libvirt plugin
sudo systemctl restart libvirtd

log "Installing Vagrant $VAGRANT_VERSION"
if have_cmd vagrant; then
    log "  $(vagrant --version) already present, skipping"
else
    deb=$(mktemp --suffix=.deb)
    trap 'rm -f "$deb"' EXIT
    wget -q --show-progress -O "$deb" "$VAGRANT_DEB_URL"
    sudo dpkg -i "$deb"
fi

log "Installing vagrant-libvirt $VAGRANT_LIBVIRT_VERSION"
if vagrant plugin list 2>/dev/null | grep -q '^vagrant-libvirt '; then
    log "  already present, skipping"
else
    vagrant plugin install vagrant-libvirt --plugin-version "$VAGRANT_LIBVIRT_VERSION"
fi

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

Start a guest from rattan/artifact/ with up.sh.

    ./up.sh micro        # microbenchmarks (§5.1)
    # or
    ./up.sh mptcp        # multipath benchmark (§5.2)

Do not use vagrant up directly. up.sh handles the required reboots and
completes provisioning.

Read README.md for the full setup and experiment workflow.

EOF
