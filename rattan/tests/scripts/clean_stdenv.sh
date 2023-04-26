#!/usr/bin/env bash
myname=${0##*/}

remove_stdenv_netns() {
    suffix=$1
    if [ -n "$suffix" ]; then
        suffix="-$suffix"
    fi
    ip netns del ns-rattan"$suffix"
    ip netns del ns-client"$suffix"
    ip netns del ns-server"$suffix"
}

remove_stdenv_netns_list() {
    for arg in "$@"; do
        remove_stdenv_netns "$arg"
    done
}

usage() {
    cat >&2 <<EOL
Clear all the netns created by stdenv.
Usage:
$myname options NETNS_NAME ...

options:
    --help|-h                   Print this help message
    --all|-a                    Clear all stdenv netns found from 'ip netns list'
    --default|-d                Clear the default stdenv netns with no suffix

Example:
    $myname ABC123 eFG456

    * this will clean all the netns named ns-rattan-ABC123, ns-client-ABC123, ns-server-ABC123,
      ns-rattan-eFG456, ns-client-eFG456, ns-server-eFG456
EOL
    exit 1
}

POSITIONAL_ARGS=()

while [[ $# -gt 0 ]]; do
    case $1 in
    --help | -h)
        usage
        ;;
    --default | -d)
        DEFAULT=true
        shift # past argument
        ;;
    --all | -a)
        REMOVE_ALL=true
        shift # past argument
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

if [ "$DEFAULT" = true ]; then
    remove_stdenv_netns
    exit 0
fi

if [ "$REMOVE_ALL" = true ]; then
    ip netns list | grep -E 'ns-(rattan|client|server)' | awk '{print $1}' | xargs -I {} ip netns del {}
    exit 0
fi

remove_stdenv_netns_list "$@"
