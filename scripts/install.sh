#!/usr/bin/env bash
myname=${0##*/}

set -e

install_rattan() {
    local binary_path=${1:-"rattan-cli"}
    install_binary "$binary_path"
    config_networkd
}

install_binary() {
    sudo install -m 755 "$1" /usr/local/bin/rattan-cli
    sudo setcap 'cap_dac_override,cap_dac_read_search,cap_sys_ptrace,cap_net_admin,cap_sys_admin,cap_net_raw+ep' /usr/local/bin/rattan-cli
}

# Config systemd-networkd to not change MAC address of veth interfaces
# Ref: https://github.com/stack-rs/rattan/issues/42
config_networkd() {
    if [ -d "/lib/systemd/network" ]; then
        sudo sh -c "cat <<EOF >/lib/systemd/network/80-rattan.link
[Match]
OriginalName=ns*-v*-*
Driver=veth

[Link]
MACAddressPolicy=none
EOF
"
        sudo systemctl daemon-reload
        sudo systemctl restart systemd-networkd.service
    fi
}

usage() {
    cat >&2 <<EOL
Install rattan-cli and grant necessary privileges
Usage:
$myname options RATTAN-CLI_PATH ...

options:
    --help|-h                   Print this help message

Example:
    $myname target/release/rattan-cli

EOL
    exit 1
}

POSITIONAL_ARGS=()

while [[ $# -gt 0 ]]; do
    case $1 in
    --help | -h)
        usage
        ;;
    --* | -*)
        echo "Unknown option $1"
        usage
        ;;
    *)
        POSITIONAL_ARGS+=("$1") # save positional arg
        shift                   # past argument
        ;;
    esac
done

set -- "${POSITIONAL_ARGS[@]}" # restore positional parameters

install_rattan "$@"
