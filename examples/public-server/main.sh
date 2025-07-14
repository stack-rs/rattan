#!/bin/bash
# This script sets up a Compatible mode Rattan and forwards a port.
myname=${0##*/}

# Enable IP forwarding
# WARNING: Please ensure the firewall allow the forwarding
# For example, if you are using UFW, you can run:
# sudo ufw route allow to 10.0.0.0/8
value=$(sysctl -n net.ipv4.ip_forward)
if [ $value -eq 0 ]; then
	echo "Enabling IP forwarding"
	sudo sysctl -w net.ipv4.ip_forward=1
fi

usage() {
	cat >&2 <<EOL
Run server in Rattan and forward the port to serve public traffic
Usage:
$myname options

options:
	--help|-h                   Print this help message
  --inner-port                Specify the port inside Rattan to use
  --outer-port                Specify the port on the host to use

Example:
    $myname --inner-port 1234 --outer-port 12345
EOL
	exit 1
}

# [Modify Here] Specify the inner and outer ports
INNER_PORT=5201
OUTER_PORT=35201

while [[ $# -gt 0 ]]; do
	case $1 in
	--help | -h)
		usage
		exit 1
		;;
	--inner-port | -i)
		INNER_PORT=$2
		shift # past argument
		shift # past value
		;;
	--outer-port | -o)
		OUTER_PORT=$2
		shift # past argument
		shift # past value
		;;
	--* | -*)
		echo "Unknown option $1"
		usage
		exit 1
		;;
	*)
		shift # past argument
		;;
	esac
done

# Get script directory
WORK_DIR=$(dirname "$(readlink -f "$0")")

# Generate a unique UUID for the script
SCRIPT_UUID=$(cat /proc/sys/kernel/random/uuid)
echo "Using UUID: $SCRIPT_UUID"

# Run Rattan in Compatible mode
rattan run -c "$WORK_DIR/rattan.toml" --left "$WORK_DIR/inner.sh" "$SCRIPT_UUID" &

# Wait for the inner script to write the IP address
while [ ! -f "/tmp/rattan_public/${SCRIPT_UUID}.addr" ]; do
	sleep 0.1
done
# Read the inner IP address
INNER_IP=$(cat "/tmp/rattan_public/${SCRIPT_UUID}.addr")
# Clean up the temporary file
rm "/tmp/rattan_public/${SCRIPT_UUID}.addr"

# Forward the port using iptables
echo "Forwarding 0.0.0.0:$OUTER_PORT -> $INNER_IP:$INNER_PORT"
sudo iptables -t nat -A PREROUTING -p tcp --dport $OUTER_PORT -j DNAT --to-destination "$INNER_IP:$INNER_PORT"
# We have default route inside the inner, so SNAT is not needed

# Clean up iptables rule on exit
cleanup() {
	echo "Cleaning up iptables rule"
	sudo iptables -t nat -D PREROUTING -p tcp --dport $OUTER_PORT -j DNAT --to-destination "$INNER_IP:$INNER_PORT"
}
trap cleanup EXIT

# Wait for Rattan
wait
