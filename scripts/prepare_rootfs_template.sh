#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF' >&2
usage: scripts/prepare_rootfs_template.sh <base-rootfs.ext4> <output-dir>

Extract a read-only ext4 rootfs image into a writable directory tree that can be
used as the --rootfs-template input to build_guest_rootfs.sh.

This script currently requires Linux plus either:
  - root, or
  - sudo access for loop mounting the ext4 image.
EOF
}

BASE_ROOTFS="${1:-}"
OUTPUT_DIR="${2:-}"
[[ -n "$BASE_ROOTFS" && -n "$OUTPUT_DIR" ]] || { usage; exit 1; }
[[ -f "$BASE_ROOTFS" ]] || { echo "base rootfs image does not exist: $BASE_ROOTFS" >&2; exit 1; }
[[ "$(uname -s)" == "Linux" ]] || { echo "prepare_rootfs_template.sh requires Linux" >&2; exit 1; }

command -v mount >/dev/null 2>&1 || { echo "mount command is required" >&2; exit 1; }
command -v umount >/dev/null 2>&1 || { echo "umount command is required" >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { echo "tar command is required" >&2; exit 1; }

use_sudo=0
if [[ "${EUID}" -ne 0 ]]; then
  command -v sudo >/dev/null 2>&1 || {
    echo "prepare_rootfs_template.sh requires root or sudo to mount loopback ext4 images" >&2
    exit 1
  }
  use_sudo=1
fi

rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

MOUNT_DIR="$(mktemp -d)"
cleanup() {
  if grep -qs " $MOUNT_DIR " /proc/mounts 2>/dev/null; then
    if [[ "$use_sudo" -eq 1 ]]; then
      sudo umount "$MOUNT_DIR" || true
    else
      umount "$MOUNT_DIR" || true
    fi
  fi
  rmdir "$MOUNT_DIR" 2>/dev/null || true
}
trap cleanup EXIT

if [[ "$use_sudo" -eq 1 ]]; then
  sudo mount -o loop,ro -t ext4 "$BASE_ROOTFS" "$MOUNT_DIR"
  sudo tar -C "$MOUNT_DIR" -cf - . | tar -C "$OUTPUT_DIR" -xf -
else
  mount -o loop,ro -t ext4 "$BASE_ROOTFS" "$MOUNT_DIR"
  tar -C "$MOUNT_DIR" -cf - . | tar -C "$OUTPUT_DIR" -xf -
fi

echo "prepared rootfs template tree at $OUTPUT_DIR"
