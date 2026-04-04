#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF' >&2
usage: scripts/fetch_official_artifacts.sh <output-dir>

Fetch the pinned Ubuntu 22.04 / Firecracker 1.12.0 artifact set used by the
first hardened release, and verify each file against the repo lock values.
EOF
}

OUT_DIR="${1:-}"
[[ -n "$OUT_DIR" ]] || { usage; exit 1; }

FC_TGZ_URL="https://github.com/firecracker-microvm/firecracker/releases/download/v1.12.0/firecracker-v1.12.0-x86_64.tgz"
FC_TGZ_SHA256="392b5f7e4bf12871d1e8377a60ed3b384a46bc2f7d3771caf202aa7a63e32676"
FC_BIN_SHA256="6ba205fa2f1ccad95848515deaee59e7750d38b7a0a49c5c805cd3097ab9f368"
KERNEL_URL="http://spec.ccfc.min.s3.amazonaws.com/firecracker-ci/v1.10/x86_64/vmlinux-5.10.223"
KERNEL_SHA256="22847375721aceea63d934c28f2dfce4670b6f52ec904fae19f5145a970c1e65"
ROOTFS_URL="http://spec.ccfc.min.s3.amazonaws.com/firecracker-ci/v1.10/x86_64/ubuntu-22.04.ext4"
ROOTFS_SHA256="040927105bd01b19e7b02cd5da5a9552b428a7f84bd5ffc22ebfce4ddf258a07"
NODE_URL="https://nodejs.org/dist/v20.20.2/node-v20.20.2-linux-x64.tar.xz"
NODE_SHA256="df770b2a6f130ed8627c9782c988fda9669fa23898329a61a871e32f965e007d"

need() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing required command: $1" >&2; exit 1; }
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

fetch() {
  local url="$1"
  local dest="$2"
  local expected="$3"
  mkdir -p "$(dirname "$dest")"
  curl -fsSL "$url" -o "$dest"
  local actual
  actual="$(sha256_file "$dest")"
  [[ "$actual" == "$expected" ]] || {
    echo "sha256 mismatch for $dest: expected $expected got $actual" >&2
    exit 1
  }
}

need curl
need tar

mkdir -p "$OUT_DIR"/firecracker "$OUT_DIR"/kernel "$OUT_DIR"/rootfs "$OUT_DIR"/node

fc_tgz="$OUT_DIR/firecracker/firecracker-v1.12.0-x86_64.tgz"
fetch "$FC_TGZ_URL" "$fc_tgz" "$FC_TGZ_SHA256"
tar -xzf "$fc_tgz" -C "$OUT_DIR/firecracker"
fc_bin="$OUT_DIR/firecracker/release-v1.12.0-x86_64/firecracker-v1.12.0-x86_64"
fc_actual="$(sha256_file "$fc_bin")"
[[ "$fc_actual" == "$FC_BIN_SHA256" ]] || {
  echo "firecracker binary sha256 mismatch: expected $FC_BIN_SHA256 got $fc_actual" >&2
  exit 1
}
ln -sfn "$(basename "$fc_bin")" "$OUT_DIR/firecracker/release-v1.12.0-x86_64/firecracker"

fetch "$KERNEL_URL" "$OUT_DIR/kernel/vmlinux-5.10.223" "$KERNEL_SHA256"
fetch "$ROOTFS_URL" "$OUT_DIR/rootfs/ubuntu-22.04.ext4" "$ROOTFS_SHA256"
fetch "$NODE_URL" "$OUT_DIR/node/node-v20.20.2-linux-x64.tar.xz" "$NODE_SHA256"

cat <<EOF
Fetched and verified:
  firecracker: $fc_bin
  firecracker shim: $OUT_DIR/firecracker/release-v1.12.0-x86_64/firecracker
  kernel: $OUT_DIR/kernel/vmlinux-5.10.223
  rootfs: $OUT_DIR/rootfs/ubuntu-22.04.ext4
  node runtime tarball: $OUT_DIR/node/node-v20.20.2-linux-x64.tar.xz
EOF
