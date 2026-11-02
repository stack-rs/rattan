#!/usr/bin/env bash
myname=${0##*/}

PERSIST_DIR=/var/run/netns

remove_stdenv_netns() {
    suffix=$1
    if [ -n "$suffix" ]; then
        suffix="-$suffix"
    fi
    ip netns del ns-rattan"$suffix"
    ip netns del ns-left"$suffix"
    ip netns del ns-right"$suffix"
}

# mountinfo records the canonical path, and /var/run is a symlink to /run
persist_mount_path() {
    readlink -f "$PERSIST_DIR" 2>/dev/null || echo "$PERSIST_DIR"
}

count_persist_mounts() {
    awk -v dir="$(persist_mount_path)" '
        $5 == dir { n++ }
        END { print n + 0 }
    ' /proc/self/mountinfo
}

# Entries that stdenv did not create, which must never be unmounted from under
# their owner. Once the directory is deeply stacked `ip netns del` starts failing
# with "Peer netns reference is invalid", so stdenv's own leftovers are expected
# here and must not block the unstacking that repairs the machine.
count_foreign_netns() {
    find "$PERSIST_DIR" -mindepth 1 -maxdepth 1 2>/dev/null |
        { grep -cvE '/ns-(rattan|left|right)(-|$)' || true; }
}

# Unstacks the mounts piled on the persist directory. `ip netns del` only unmounts
# /var/run/netns/<name>, never the directory itself, so a crashed run leaves layers
# that make every later netns cost one mount entry per layer.
clean_persist_mounts() {
    local before after remaining
    before=$(count_persist_mounts)

    remaining=$(find "$PERSIST_DIR" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l)
    if [ "$remaining" -gt 0 ] && [ "$FORCE" != true ]; then
        echo "$myname: $PERSIST_DIR still holds $remaining netns; refusing to unmount." >&2
        echo "$myname: clear them first, or pass --force if they are known to be stale." >&2
        return 1
    fi

    while [ "$(count_persist_mounts)" -gt 0 ]; do
        umount "$PERSIST_DIR" 2>/dev/null ||
            umount -l "$PERSIST_DIR" 2>/dev/null ||
            break
    done

    after=$(count_persist_mounts)
    echo "$myname: $PERSIST_DIR mounts $before -> $after"
    # leftover placeholder files from netns creations that failed after ENOSPC
    find "$PERSIST_DIR" -mindepth 1 -maxdepth 1 -name 'ns-*' -delete 2>/dev/null
    [ "$after" -eq 0 ]
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
    --all|-a                    Clear all stdenv netns found from 'ip netns list',
                                then unstack $PERSIST_DIR
    --default|-d                Clear the default stdenv netns with no suffix
    --mounts|-m                 Only unstack the mounts piled on $PERSIST_DIR
    --force|-f                  Unstack even while $PERSIST_DIR still holds netns

Example:
    $myname ABC123 eFG456

    * this will clean all the netns named ns-rattan-ABC123, ns-left-ABC123, ns-right-ABC123,
      ns-rattan-eFG456, ns-left-eFG456, ns-right-eFG456

    $myname --mounts

    * this repairs a worker whose tasks fail with ENOSPC while mounting a netns:
      check the layer count with
          awk -v d="\$(readlink -f $PERSIST_DIR)" '\$5 == d' /proc/self/mountinfo | wc -l
      a healthy machine reports 1
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
    --mounts | -m)
        CLEAN_MOUNTS=true
        shift # past argument
        ;;
    --force | -f)
        FORCE=true
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

if [ "$CLEAN_MOUNTS" = true ]; then
    clean_persist_mounts
    exit $?
fi

if [ "$DEFAULT" = true ]; then
    remove_stdenv_netns
    exit 0
fi

if [ "$REMOVE_ALL" = true ]; then
    rm -rf /tmp/rattan/
    ip netns list | grep -E 'ns-(rattan|left|right)' | awk '{print $1}' | xargs -I {} ip netns del {}
    # only stdenv leftovers remain, so unstack without demanding --force
    if [ "$(count_foreign_netns)" -eq 0 ]; then
        FORCE=true
    fi
    clean_persist_mounts || true
    exit 0
fi

remove_stdenv_netns_list "$@"
