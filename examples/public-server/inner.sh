#!/bin/bash

# Get the script UUID from outer script
SCRIPT_UUID=$1
# Get script directory
WORK_DIR=$(dirname "$(readlink -f "$0")")

# Get Rattan inner IP
INTERFACE=$(ip route | grep default | awk '{print $5}')
CURRENT_IP=$(ip addr show "$INTERFACE" | grep 'inet ' | awk '{print $2}' | cut -d'/' -f1)

# Write the inner IP to a temporary file
mkdir -p /tmp/rattan_public
echo "$CURRENT_IP" >/tmp/rattan_public/"$SCRIPT_UUID".addr

# TODO: [Modify Here or the run.sh script]
# Run your app here (block run)
echo "Starting Application on $CURRENT_IP (inner Rattan)"
bash "$WORK_DIR/run.sh"
