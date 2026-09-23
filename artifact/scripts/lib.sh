# shellcheck shell=bash

log() { printf '[%s] %s\n' "${LOG_TAG:-artifact}" "$*"; }

warn() { printf '[%s] warning: %s\n' "${LOG_TAG:-artifact}" "$*" >&2; }

die() {
    printf '[%s] error: %s\n' "${LOG_TAG:-artifact}" "$*" >&2
    exit 1
}

have_cmd() { command -v "$1" >/dev/null 2>&1; }

require_cmd() {
    for cmd in "$@"; do
        have_cmd "$cmd" || die "$cmd is not installed. Run the VM setup script first."
    done
}

require_not_root() {
    [ "$(id -u)" -ne 0 ] || die "Run this as a normal user, not root. It calls sudo where needed."
}

keep_sudo_alive() {
    sudo -v
    (
        while sudo -n true 2>/dev/null; do sleep 60; done
    ) &
    local pid=$!
    # shellcheck disable=SC2064
    trap "kill $pid 2>/dev/null || true" EXIT
}

apt_install() {
    if [ -z "${_APT_UPDATED:-}" ]; then
        sudo DEBIAN_FRONTEND=noninteractive apt-get -qq update
        _APT_UPDATED=1
    fi
    sudo DEBIAN_FRONTEND=noninteractive apt-get -qq install -y "$@"
}

retry() {
    local attempts=${RETRY_ATTEMPTS:-3} n=1
    until "$@"; do
        if [ "$n" -ge "$attempts" ]; then
            die "gave up after $attempts attempts: $*"
        fi
        warn "attempt $n of $attempts failed, retrying: $*"
        n=$((n + 1))
        sleep 3
    done
}

clone_at() {
    local url=$1 commit=$2 dir=$3
    if [ -d "$dir/.git" ]; then
        log "  $dir already cloned"
    else
        retry git clone --quiet "$url" "$dir"
    fi
    git -C "$dir" fetch --quiet origin "$commit" 2>/dev/null || retry git -C "$dir" fetch --quiet origin
    git -C "$dir" checkout --quiet --detach "$commit"
}

grow_partition() {
    local part=$1 disk number
    disk=$(lsblk -no PKNAME "$part" 2>/dev/null | head -1)
    if [ -z "$disk" ]; then
        warn "  cannot tell which disk $part is on, leaving it alone"
        return
    fi
    number=${part#"/dev/$disk"}
    number=${number#p}
    sudo growpart "/dev/$disk" "$number" >/dev/null 2>&1 || true
}

grow_root_fs() {
    local before after root_src pv
    before=$(df --output=size -BG / | tail -1 | tr -dc '0-9')
    root_src=$(findmnt -no SOURCE /)

    if [[ "$root_src" == /dev/mapper/* ]]; then
        pv=$(sudo pvs --noheadings -o pv_name 2>/dev/null | head -1 | tr -d ' ')
        if [ -n "$pv" ]; then
            grow_partition "$pv"
            sudo pvresize "$pv" >/dev/null 2>&1 || true
        fi
        sudo lvextend -l +100%FREE "$root_src" >/dev/null 2>&1 || true
    else
        grow_partition "$(realpath "$root_src")"
    fi
    sudo resize2fs "$root_src" >/dev/null 2>&1 || true

    after=$(df --output=size -BG / | tail -1 | tr -dc '0-9')
    log "  root filesystem: ${before}G -> ${after}G"
}

cpu_count() { getconf _NPROCESSORS_ONLN; }

toml_strings() {
    local out=""
    local item
    for item in "$@"; do
        out+="${out:+, }\"$item\""
    done
    printf '[%s]' "$out"
}
