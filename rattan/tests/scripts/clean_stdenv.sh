#!/usr/bin/env bash
suffix=$1

if [ -n "$suffix" ]; then
    suffix="-$suffix"
fi
ip netns del ns-rattan"$suffix"
ip netns del ns-client"$suffix"
ip netns del ns-server"$suffix"

