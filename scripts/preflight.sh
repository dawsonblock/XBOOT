#!/usr/bin/env bash
set -euo pipefail

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 1; }; }
need curl
need sha256sum
need python3

[[ -e /dev/kvm ]] || { echo "/dev/kvm missing" >&2; exit 1; }
[[ -r /dev/kvm && -w /dev/kvm ]] || echo "warning: current user may not be able to use /dev/kvm" >&2

if [[ -n "${ZEROBOOT_ALLOWED_FIRECRACKER_VERSION:-}" ]]; then
  need firecracker
  actual_fc="$(firecracker --version 2>/dev/null || true)"
  [[ "$actual_fc" == "$ZEROBOOT_ALLOWED_FIRECRACKER_VERSION" ]] || {
    echo "firecracker version mismatch: expected '$ZEROBOOT_ALLOWED_FIRECRACKER_VERSION', got '$actual_fc'" >&2
    exit 1
  }
fi

for p in "$@"; do
  [[ -e "$p" ]] || { echo "missing artifact: $p" >&2; exit 1; }
  sha256sum "$p"
done

echo "preflight ok"

if [[ -n "${ZEROBOOT_WORKDIR:-}" ]] && [[ -f "$ZEROBOOT_WORKDIR/template.manifest.json" ]]; then
  python3 scripts/validate_template_manifest.py "$ZEROBOOT_WORKDIR"
fi
