#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<EOF >&2
usage: $0 <staging-dir> <output.ext4> [size-mib]

Build an ext4 image from a prepared staging tree using mkfs.ext4 -d.
This script does not require mounting the image as root, but it does require
mkfs.ext4 with directory population support.
EOF
}

STAGING_DIR="${1:-}"
OUTPUT_IMAGE="${2:-}"
SIZE_MIB="${3:-256}"
[[ -n "$STAGING_DIR" && -n "$OUTPUT_IMAGE" ]] || { usage; exit 1; }
[[ -d "$STAGING_DIR" ]] || { echo "staging dir does not exist: $STAGING_DIR" >&2; exit 1; }
[[ "$SIZE_MIB" =~ ^[0-9]+$ ]] || { echo "size-mib must be an integer" >&2; exit 1; }
command -v mkfs.ext4 >/dev/null 2>&1 || { echo "mkfs.ext4 not found" >&2; exit 1; }

mkdir -p "$(dirname "$OUTPUT_IMAGE")"
rm -f "$OUTPUT_IMAGE"
truncate -s "$((SIZE_MIB * 1024 * 1024))" "$OUTPUT_IMAGE"
mkfs.ext4 -q -F -d "$STAGING_DIR" "$OUTPUT_IMAGE"
sha256sum "$OUTPUT_IMAGE" > "$OUTPUT_IMAGE.sha256"
echo "built rootfs image: $OUTPUT_IMAGE"
echo "wrote checksum: $OUTPUT_IMAGE.sha256"
