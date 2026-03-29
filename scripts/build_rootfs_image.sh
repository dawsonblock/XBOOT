#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<EOF >&2
usage: $0 <staging-dir> <output.ext4> [size-mib]

Build an ext4 image from a prepared staging tree using mkfs.ext4 -d.
This script does not require mounting the image as root, but it does require
mkfs.ext4 with directory population support. When size-mib is omitted, the
script picks a minimum image size based on the staging tree contents.
EOF
}

STAGING_DIR="${1:-}"
OUTPUT_IMAGE="${2:-}"
SIZE_MIB="${3:-}"
[[ -n "$STAGING_DIR" && -n "$OUTPUT_IMAGE" ]] || { usage; exit 1; }
[[ -d "$STAGING_DIR" ]] || { echo "staging dir does not exist: $STAGING_DIR" >&2; exit 1; }
if [[ -n "$SIZE_MIB" && ! "$SIZE_MIB" =~ ^[0-9]+$ ]]; then
  echo "size-mib must be an integer" >&2
  exit 1
fi
command -v mkfs.ext4 >/dev/null 2>&1 || { echo "mkfs.ext4 not found" >&2; exit 1; }

staging_kib="$(du -sk "$STAGING_DIR" | awk '{print $1}')"
overhead_kib="$(( staging_kib / 5 ))"
if (( overhead_kib < 131072 )); then
  overhead_kib=131072
fi
minimum_kib="$(( staging_kib + overhead_kib ))"
minimum_mib="$(( (minimum_kib + 1023) / 1024 ))"

if [[ -z "$SIZE_MIB" ]]; then
  SIZE_MIB="$minimum_mib"
elif (( SIZE_MIB < minimum_mib )); then
  echo "requested image size ${SIZE_MIB} MiB is too small for $STAGING_DIR; minimum is ${minimum_mib} MiB" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUTPUT_IMAGE")"
rm -f "$OUTPUT_IMAGE"
truncate -s "$((SIZE_MIB * 1024 * 1024))" "$OUTPUT_IMAGE"
mkfs.ext4 -q -F -d "$STAGING_DIR" "$OUTPUT_IMAGE"
sha256sum "$OUTPUT_IMAGE" > "$OUTPUT_IMAGE.sha256"
echo "built rootfs image: $OUTPUT_IMAGE"
echo "size: ${SIZE_MIB} MiB"
echo "wrote checksum: $OUTPUT_IMAGE.sha256"
