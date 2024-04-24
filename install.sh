#!/bin/bash

set -e

# Config systemd-networkd to not change MAC address of veth interfaces
# Ref: https://github.com/stack-rs/rattan/issues/42

if [ -d "/lib/systemd/network" ]; then
    cat <<EOF >/lib/systemd/network/80-rattan.link
[Match]
OriginalName=ns*-v*-*
Driver=veth

[Link]
MACAddressPolicy=none
EOF

    systemctl daemon-reload
    systemctl restart systemd-networkd.service
fi
