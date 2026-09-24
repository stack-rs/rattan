#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$(realpath "$0")")"
# shellcheck source=lib.sh
source lib.sh
# shellcheck source=versions.sh
source versions.sh

: "${LOG_TAG:=setup:mptcp-kernel}"
require_not_root

CONFIG="$(realpath mptcp-kernel.config)"
SOURCE_DIR="$TOOLS_DIR/mptcp-v0.96"

if [ "$(uname -r)" = "$MPTCP_KERNEL_RELEASE" ]; then
    log "Already running $MPTCP_KERNEL_RELEASE, nothing to build"
    exit 0
fi

log "Installing what the kernel build needs"
apt_install \
    build-essential bc flex bison libelf-dev libssl-dev \
    cpio zstd kmod rsync git

if [ -d "$SOURCE_DIR/.git" ] &&
    [ "$(git -C "$SOURCE_DIR" rev-parse HEAD)" = "$MPTCP_COMMIT" ]; then
    log "Source already at $MPTCP_COMMIT"
else
    log "Fetching the MPTCP v0.96 source (a few hundred megabytes)"
    mkdir -p "$SOURCE_DIR"
    git -C "$SOURCE_DIR" init --quiet
    git -C "$SOURCE_DIR" remote add origin "$MPTCP_REPO" 2>/dev/null || true
    retry git -C "$SOURCE_DIR" fetch --quiet --depth 1 origin "$MPTCP_COMMIT"
    git -C "$SOURCE_DIR" checkout --quiet --detach FETCH_HEAD
fi

cd "$SOURCE_DIR"
[ -f net/mptcp/mptcp_ctrl.c ] ||
    die "this does not look like the MPTCP fork: net/mptcp is missing"

# An empty LOCALVERSION keeps the release exactly $MPTCP_KERNEL_RELEASE.
KERNEL_MAKE=(make "LOCALVERSION=")

log "Configuring"
cp "$CONFIG" .config
"${KERNEL_MAKE[@]}" -s olddefconfig

release=$("${KERNEL_MAKE[@]}" -s kernelrelease)
[ "$release" = "$MPTCP_KERNEL_RELEASE" ] ||
    die "the source calls itself '$release', not '$MPTCP_KERNEL_RELEASE'.
  MPTCP_COMMIT in versions.sh has probably moved to a different stable release."

log "Building $release on $(cpu_count) CPUs, about 10 minutes on 40"
"${KERNEL_MAKE[@]}" -j"$(cpu_count)"

log "Installing the modules"
sudo "${KERNEL_MAKE[@]}" -s modules_install

log "Installing the kernel and building its initramfs"
sudo "${KERNEL_MAKE[@]}" -s install

[ -f "/boot/vmlinuz-$release" ] || die "make install left no /boot/vmlinuz-$release"
[ -f "/boot/initrd.img-$release" ] ||
    die "no /boot/initrd.img-$release; the initramfs hook did not run"

log "Pointing GRUB at $release"
sudo update-grub 2>&1 | sed 's/^/  /'

entry_id=$(sudo grep -om1 "gnulinux-$release-advanced-[^']*" /boot/grub/grub.cfg || true)
[ -n "$entry_id" ] ||
    die "cannot find a GRUB menu entry for $release in /boot/grub/grub.cfg"

submenu_id=$(sudo grep -om1 "gnulinux-advanced-[^']*" /boot/grub/grub.cfg || true)
default="${submenu_id:+$submenu_id>}$entry_id"
if grep -q '^GRUB_DEFAULT=' /etc/default/grub; then
    sudo sed -i "s|^GRUB_DEFAULT=.*|GRUB_DEFAULT=\"$default\"|" /etc/default/grub
else
    echo "GRUB_DEFAULT=\"$default\"" | sudo tee -a /etc/default/grub >/dev/null
fi
sudo update-grub 2>&1 | sed 's/^/  /'

log "  GRUB_DEFAULT=\"$default\""

cat <<EOF

Kernel $release installed. Reboot for it to take effect:

    vagrant reload mptcp

and then run the second half of the setup:

    vagrant provision mptcp --provision-with expr

\`up.sh mptcp\` does both of those for you.
EOF
