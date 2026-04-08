#!/usr/bin/env bash
myname=${0##*/}

set -e

GITHUB_REPO="stack-rs/rattan"
APP_NAME="rattan"

# --- Utility functions ---

err() {
  local red reset
  red=$(tput setaf 1 2>/dev/null || echo '')
  reset=$(tput sgr0 2>/dev/null || echo '')
  echo "${red}ERROR${reset}: $1" >&2
  exit 1
}

warn() {
  local yellow reset
  yellow=$(tput setaf 3 2>/dev/null || echo '')
  reset=$(tput sgr0 2>/dev/null || echo '')
  echo "${yellow}WARN${reset}: $1" >&2
}

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    err "need '$1' (command not found)"
  fi
}

# This wraps curl or wget. Try curl first, if not installed, use wget instead.
downloader() {
  local _dld
  local _snap_curl=0

  if command -v curl >/dev/null 2>&1; then
    if echo "$(command -v curl)" | grep -q '/snap/'; then
      _snap_curl=1
    fi
  fi

  if command -v curl >/dev/null 2>&1 && [ "$_snap_curl" = "0" ]; then
    _dld=curl
  elif command -v wget >/dev/null 2>&1; then
    _dld=wget
  elif [ "$_snap_curl" = "1" ]; then
    err "curl installed via snap cannot download files due to missing permissions. Please reinstall curl with apt or another package manager."
  else
    err "need 'curl' or 'wget' (command not found)"
  fi

  if [ "$_dld" = "curl" ]; then
    curl -sSfL "$1" -o "$2"
  elif [ "$_dld" = "wget" ]; then
    wget -q "$1" -O "$2"
  fi
}

# --- Architecture detection ---
check_glibc() {
  local _min_major="$1"
  local _min_minor="$2"
  local _local_glibc

  _local_glibc="$(ldd --version 2>/dev/null | awk -F' ' 'FNR==1 { print $NF }')"
  if [ -z "$_local_glibc" ]; then
    return 1
  fi

  local _major _minor
  _major="$(echo "$_local_glibc" | awk -F. '{ print $1 }')"
  _minor="$(echo "$_local_glibc" | awk -F. '{ print $2 }')"

  if [ "$_major" = "$_min_major" ] && [ "$_minor" -ge "$_min_minor" ] 2>/dev/null; then
    return 0
  else
    warn "System glibc version ('${_local_glibc}') may be too old; trying musl variant"
    return 1
  fi
}

get_target_triple() {
  local _ostype _cputype _clibtype

  need_cmd uname

  _ostype="$(uname -s)"
  _cputype="$(uname -m)"

  if [ "$_ostype" != "Linux" ]; then
    err "$APP_NAME only supports Linux (requires network namespaces and XDP). Detected OS: $_ostype"
  fi

  # Detect musl vs glibc
  if ldd --version 2>&1 | grep -q 'musl'; then
    _clibtype="musl"
  else
    _clibtype="gnu"
  fi

  case "$_cputype" in
  x86_64 | x86-64 | x64 | amd64)
    _cputype=x86_64
    ;;
  aarch64 | arm64)
    _cputype=aarch64
    ;;
  *)
    err "Unsupported CPU architecture: $_cputype. Only x86_64 and aarch64 are supported."
    ;;
  esac

  # For glibc targets, check the minimum required version.
  # Fall back to musl if glibc is too old.
  if [ "$_clibtype" = "gnu" ]; then
    if ! check_glibc "2" "39"; then
      _clibtype="musl"
    fi
  fi

  echo "${_cputype}-unknown-linux-${_clibtype}"
}

# --- GitHub release download ---
get_latest_release_tag() {
  local _api_url="https://api.github.com/repos/${GITHUB_REPO}/releases/latest"
  local _tag

  if command -v curl >/dev/null 2>&1; then
    _tag="$(curl -sSfL "$_api_url" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
  elif command -v wget >/dev/null 2>&1; then
    _tag="$(wget -qO- "$_api_url" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
  else
    err "need 'curl' or 'wget' to fetch the latest release"
  fi

  if [ -z "$_tag" ]; then
    err "Failed to determine the latest release tag from GitHub. Check your network connection."
  fi

  echo "$_tag"
}

download_rattan() {
  need_cmd uname
  need_cmd mktemp
  need_cmd tar

  local _target
  _target="$(get_target_triple)"
  echo "Detected platform: $_target" >&2

  local _tag
  _tag="$(get_latest_release_tag)"
  echo "Latest release: $_tag" >&2

  local _archive="${APP_NAME}-${_target}.tar.xz"
  local _url="https://github.com/${GITHUB_REPO}/releases/download/${_tag}/${_archive}"

  local _dir
  _dir="$(mktemp -d)"
  local _file="${_dir}/${_archive}"

  echo "Downloading ${_url} ..." >&2
  if ! downloader "$_url" "$_file"; then
    # Try .tar.gz if .tar.xz fails
    _archive="${APP_NAME}-${_target}.tar.gz"
    _url="https://github.com/${GITHUB_REPO}/releases/download/${_tag}/${_archive}"
    _file="${_dir}/${_archive}"
    echo "Retrying with ${_url} ..." >&2
    if ! downloader "$_url" "$_file"; then
      rm -rf "$_dir"
      err "Failed to download release archive. No compatible release found for platform '${_target}'."
    fi
  fi

  echo "Extracting archive..." >&2
  tar xf "$_file" --strip-components 1 -C "$_dir"

  local _binary="${_dir}/${APP_NAME}"
  if [ ! -f "$_binary" ]; then
    rm -rf "$_dir"
    err "Binary '${APP_NAME}' not found in extracted archive."
  fi

  echo "$_binary"
  # Caller is responsible for cleanup; pass dir via global so we can rm it after install
  _DOWNLOAD_TMPDIR="$_dir"
}

# --- Install steps ---
install_binary() {
  sudo install -m 755 "$1" /usr/local/bin/rattan
  sudo setcap 'cap_dac_override,cap_dac_read_search,cap_sys_ptrace,cap_net_admin,cap_sys_admin,cap_net_raw+ep' /usr/local/bin/rattan
}

# Config systemd-networkd to not change MAC address of veth interfaces
# Ref: https://github.com/stack-rs/rattan/issues/42
config_networkd() {
  if [ -d "/lib/systemd/network" ]; then
    if [ -f "/lib/systemd/network/80-rattan.link" ]; then
      return
    fi
    sudo sh -c "cat <<EOF >/lib/systemd/network/80-rattan.link
[Match]
OriginalName=ns*-v*-*
Driver=veth

[Link]
MACAddressPolicy=none
EOF
"
    sudo systemctl daemon-reload
    sudo systemctl restart systemd-networkd.service
  fi
}

install_rattan() {
  local _binary_path="$1"
  install_binary "$_binary_path"
  config_networkd
  echo "$APP_NAME installed successfully to /usr/local/bin/$APP_NAME"
}

# --- Entry point ---

usage() {
  cat >&2 <<EOL
Install rattan and grant necessary capabilities

Usage:
  $myname [options] [RATTAN_BINARY_PATH]

If RATTAN_BINARY_PATH is not provided, the latest release is downloaded
automatically from https://github.com/${GITHUB_REPO}/releases

Options:
    --help|-h    Print this help message

Examples:
    $myname                         # Download and install latest release
    $myname target/release/rattan   # Install a locally-built binary

EOL
  exit 1
}

POSITIONAL_ARGS=()
while [[ $# -gt 0 ]]; do
  case $1 in
  --help | -h)
    usage
    ;;
  --* | -*)
    echo "Unknown option $1"
    usage
    ;;
  *)
    POSITIONAL_ARGS+=("$1")
    shift
    ;;
  esac
done

set -- "${POSITIONAL_ARGS[@]+"${POSITIONAL_ARGS[@]}"}"

_DOWNLOAD_TMPDIR=""

if [[ $# -gt 0 ]]; then
  # Local binary provided
  install_rattan "$1"
else
  # Download from GitHub
  _binary="$(download_rattan)"
  install_rattan "$_binary"
  if [ -n "$_DOWNLOAD_TMPDIR" ]; then
    rm -rf "$_DOWNLOAD_TMPDIR"
  fi
fi
