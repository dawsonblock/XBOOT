#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<EOF >&2
usage: $0 <staging-dir> <output.ext4> [size-mib]

Build an ext4 image from a prepared staging tree using mkfs.ext4 -d.
This script does not require mounting the image as root, but it does require
mkfs.ext4 with directory population support.

If size-mib is omitted, the script computes a minimum size from the staging tree
plus headroom. If size-mib is provided but too small, the script fails with the
minimum required size.
EOF
}

STAGING_DIR="${1:-}"
OUTPUT_IMAGE="${2:-}"
SIZE_MIB="${3:-}"
[[ -n "$STAGING_DIR" && -n "$OUTPUT_IMAGE" ]] || { usage; exit 1; }
[[ -d "$STAGING_DIR" ]] || { echo "staging dir does not exist: $STAGING_DIR" >&2; exit 1; }
command -v mkfs.ext4 >/dev/null 2>&1 || { echo "mkfs.ext4 not found" >&2; exit 1; }
command -v du >/dev/null 2>&1 || { echo "du not found" >&2; exit 1; }

calc_min_size_mib() {
  local du_kib base_mib min_mib
  du_kib="$(du -sk "$STAGING_DIR" | awk '{print $1}')"
  base_mib="$(( (du_kib + 1023) / 1024 ))"
  min_mib="$(( base_mib + 512 ))"
  echo $(( ((min_mib + 127) / 128) * 128 ))
}

MIN_SIZE_MIB="$(calc_min_size_mib)"
if [[ -n "$SIZE_MIB" ]]; then
  [[ "$SIZE_MIB" =~ ^[0-9]+$ ]] || { echo "size-mib must be an integer" >&2; exit 1; }
  if (( SIZE_MIB < MIN_SIZE_MIB )); then
    echo "size-mib $SIZE_MIB is too small for $STAGING_DIR; need at least $MIN_SIZE_MIB MiB" >&2
    exit 1
  fi
else
  SIZE_MIB="$MIN_SIZE_MIB"
fi

mkdir -p "$(dirname "$OUTPUT_IMAGE")"
rm -f "$OUTPUT_IMAGE"
truncate -s "$((SIZE_MIB * 1024 * 1024))" "$OUTPUT_IMAGE"
mkfs.ext4 -q -F -d "$STAGING_DIR" "$OUTPUT_IMAGE"
sha256sum "$OUTPUT_IMAGE" > "$OUTPUT_IMAGE.sha256"
echo "built rootfs image: $OUTPUT_IMAGE"
echo "wrote checksum: $OUTPUT_IMAGE.sha256"
echo "size_mib: $SIZE_MIB"
