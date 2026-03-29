#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF' >&2
usage: scripts/prepare_rootfs_template.sh <input.ext4> <output-dir>

Extract a pinned ext4 rootfs image into a directory tree that can be passed to
`build_guest_rootfs.sh --rootfs-template ...`.

This script requires a Linux host with loop-mount support. When not run as
root, it uses `sudo` for the mount, copy, and unmount steps so special files
are preserved.
EOF
}

ROOTFS_IMAGE="${1:-}"
OUTPUT_DIR="${2:-}"
[[ -n "$ROOTFS_IMAGE" && -n "$OUTPUT_DIR" ]] || { usage; exit 1; }
[[ -f "$ROOTFS_IMAGE" ]] || { echo "rootfs image not found: $ROOTFS_IMAGE" >&2; exit 1; }

case "$(uname -s)" in
  Linux) ;;
  *)
    echo "prepare_rootfs_template.sh requires Linux loop-mount support" >&2
    exit 1
    ;;
esac

need() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing required command: $1" >&2; exit 1; }
}

need mount
need umount
need tar
need mktemp

SUDO=""
if [[ "$(id -u)" -ne 0 ]]; then
  need sudo
  SUDO="sudo"
fi

TMP_BASE="$(mktemp -d)"
MOUNT_DIR="$TMP_BASE/mount"
mkdir -p "$MOUNT_DIR"

cleanup() {
  if mountpoint -q "$MOUNT_DIR" 2>/dev/null; then
    $SUDO umount "$MOUNT_DIR" || true
  fi
  rm -rf "$TMP_BASE"
}
trap cleanup EXIT

rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

$SUDO mount -o loop,ro -t ext4 "$ROOTFS_IMAGE" "$MOUNT_DIR"

if [[ -n "$SUDO" ]]; then
  $SUDO tar -C "$MOUNT_DIR" -cf - . | $SUDO tar \
    --delay-directory-restore \
    --no-same-owner \
    --no-same-permissions \
    -C "$OUTPUT_DIR" \
    -xf -
  $SUDO chown -R "$(id -u):$(id -g)" "$OUTPUT_DIR"
else
  tar -C "$MOUNT_DIR" -cf - . | tar \
    --delay-directory-restore \
    --no-same-owner \
    --no-same-permissions \
    -C "$OUTPUT_DIR" \
    -xf -
fi

cat <<EOF
Prepared rootfs template tree:
  image:  $ROOTFS_IMAGE
  output: $OUTPUT_DIR
EOF
