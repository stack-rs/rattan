#!/usr/bin/env bash

iter=$1
server_ip=$2
echo "Server IP: $server_ip"

echo "Pre running"
ping $server_ip -i 0.2 -c 5 >/dev/null 2>&1

echo "Running"
ping $server_ip -i 0.2 -c $iter
sleep 1
