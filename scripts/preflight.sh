#!/usr/bin/env bash
set -euo pipefail

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 1; }; }

resolve_firecracker_bin() {
  local candidate="${ZEROBOOT_FIRECRACKER_BIN:-firecracker}"
  if [[ "$candidate" == */* ]]; then
    [[ -x "$candidate" ]] || {
      echo "firecracker binary is not executable: $candidate" >&2
      exit 1
    }
    printf '%s\n' "$candidate"
    return 0
  fi

  command -v "$candidate" >/dev/null 2>&1 || {
    echo "missing firecracker binary: $candidate" >&2
    exit 1
  }
  command -v "$candidate"
}

need curl
need sha256sum
need python3

[[ -e /dev/kvm ]] || { echo "/dev/kvm missing" >&2; exit 1; }
[[ -r /dev/kvm && -w /dev/kvm ]] || echo "warning: current user may not be able to use /dev/kvm" >&2

if [[ -n "${ZEROBOOT_MIN_FREE_BYTES:-}" || -n "${ZEROBOOT_MIN_FREE_INODES:-}" ]]; then
  target="${ZEROBOOT_PREFLIGHT_PATH:-.}"
  read -r free_blocks < <(df -Pk "$target" | awk 'NR==2 {print $4}')
  read -r free_inodes < <(df -Pi "$target" | awk 'NR==2 {print $4}')
  free_bytes=$((free_blocks * 1024))
  min_free_bytes="${ZEROBOOT_MIN_FREE_BYTES:-0}"
  min_free_inodes="${ZEROBOOT_MIN_FREE_INODES:-0}"
  if (( free_bytes < min_free_bytes )); then
    echo "free-space watermark violation at $target: $free_bytes < $min_free_bytes" >&2
    exit 1
  fi
  if (( free_inodes < min_free_inodes )); then
    echo "free-inode watermark violation at $target: $free_inodes < $min_free_inodes" >&2
    exit 1
  fi
fi

fc_bin=""
if [[ -n "${ZEROBOOT_ALLOWED_FIRECRACKER_VERSION:-}" ]]; then
  fc_bin="$(resolve_firecracker_bin)"
  actual_fc="$($fc_bin --version 2>&1 || true)"
  [[ "$actual_fc" == "$ZEROBOOT_ALLOWED_FIRECRACKER_VERSION" ]] || {
    echo "firecracker version mismatch: expected '$ZEROBOOT_ALLOWED_FIRECRACKER_VERSION', got '$actual_fc'" >&2
    exit 1
  }
fi

if [[ -n "${ZEROBOOT_ALLOWED_FC_BINARY_SHA256:-}" ]]; then
  if [[ -z "$fc_bin" ]]; then
    fc_bin="$(resolve_firecracker_bin)"
  fi
  actual_sha="$(sha256sum "$fc_bin" | awk '{print $1}')"
  [[ "$actual_sha" == "${ZEROBOOT_ALLOWED_FC_BINARY_SHA256,,}" ]] || {
    echo "firecracker sha mismatch: expected '$ZEROBOOT_ALLOWED_FC_BINARY_SHA256', got '$actual_sha'" >&2
    exit 1
  }
fi

if [[ ! -e /sys/fs/cgroup/cgroup.controllers ]]; then
  echo "unsupported cgroup mode: expected cgroup v2" >&2
  exit 1
fi

for p in "$@"; do
  [[ -e "$p" ]] || { echo "missing artifact: $p" >&2; exit 1; }
  sha256sum "$p"
done

echo "preflight ok"

if [[ -n "${ZEROBOOT_WORKDIR:-}" ]] && [[ -f "$ZEROBOOT_WORKDIR/template.manifest.json" ]]; then
  python3 scripts/validate_template_manifest.py "$ZEROBOOT_WORKDIR"
fi
