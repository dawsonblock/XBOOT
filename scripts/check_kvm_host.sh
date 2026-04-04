#!/usr/bin/env bash
set -euo pipefail

fail() {
  echo "$1" >&2
  exit 1
}

[[ "$(uname -s)" == "Linux" ]] || fail "host OS must be Linux"
[[ "$(uname -m)" == "x86_64" ]] || fail "host arch must be x86_64"

if [[ -f /etc/os-release ]]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  [[ "${ID:-}" == "ubuntu" && "${VERSION_ID:-}" == "22.04" ]] || \
    fail "host distro must be Ubuntu 22.04"
fi

[[ -e /dev/kvm ]] || fail "/dev/kvm missing"
[[ -r /dev/kvm && -w /dev/kvm ]] || fail "/dev/kvm must be readable and writable by the current user"
[[ -e /sys/fs/cgroup/cgroup.controllers ]] || fail "unsupported cgroup mode: expected cgroup v2"

echo "kvm host ok"
