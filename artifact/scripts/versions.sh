# shellcheck shell=bash
# shellcheck disable=SC2034

VAGRANT_VERSION='2.4.9'
VAGRANT_LIBVIRT_VERSION='0.12.2'
# 0.13 dropped libvirt_ip_command, which vagrant-libvirt 0.12.2 still passes
FOG_LIBVIRT_VERSION='0.12.2'

MAHIMAHI_REPO='https://github.com/BobAnkh/mahimahi-prune.git'
MAHIMAHI_COMMIT='3ff069d2f72ca186f3b01fde37a177924ba34739'

MININET_REPO='https://github.com/mininet/mininet.git'
MININET_COMMIT='6eb8973c0bfd13c25c244a3871130c5e36b5fbd7'

MPTCP_REPO='https://github.com/multipath-tcp/mptcp.git'
MPTCP_COMMIT='68d3cd872201aa95c965e31b7151debbfb0bb11c'
MPTCP_KERNEL_RELEASE='5.4.301'

TOOLS_DIR="$HOME/tools"
