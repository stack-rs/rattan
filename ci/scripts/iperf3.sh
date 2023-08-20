#!/usr/bin/env bash

iter=$1
server_ip=$2
cc=$3
echo "Server IP: $server_ip"

echo "Pre running"
ping $server_ip -i 0.2 -c 5 >/dev/null 2>&1

echo "Running"
iperf3 -c $server_ip -t $iter -f m -C $cc
sleep 1
