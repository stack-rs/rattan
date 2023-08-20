#!/usr/bin/env bash

iter=$1
server_ip=$2
echo "Server IP: $server_ip"

echo "Setting arp gc_stale_time to 3600"
echo 3600 | tee /proc/sys/net/ipv4/neigh/*/gc_stale_time >/dev/null
echo 1800 | tee /proc/sys/net/ipv4/neigh/*/base_reachable_time >/dev/null
touch /var/run/netns/default
mount --bind /proc/1/ns/net /var/run/netns/default
ip netns exec default bash -c "echo 3600 | tee /proc/sys/net/ipv4/neigh/rs-*/gc_stale_time >/dev/null"
ip netns exec default bash -c "echo 1800 | tee /proc/sys/net/ipv4/neigh/rs-*/base_reachable_time >/dev/null"
umount /var/run/netns/default
rm /var/run/netns/default

# echo "Starting tcpdump"
# sudo tcpdump -w $server_ip.pcap &
# tcpdump_pid=$!
# sleep 1

echo "Pre running"
ping $server_ip -i 0.2 -c 5 >/dev/null 2>&1

echo "Running"
ping $server_ip -i 0.2 -c $iter
sleep 1

# echo "Stopping tcpdump"
# sudo kill -2 $tcpdump_pid
