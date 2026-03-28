#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT="${1:-$ROOT/dist/zeroboot-source-bundle.tar.gz}"
TMPDIR="$(mktemp -d)"
STAGE="$TMPDIR/zeroboot"

mkdir -p "$STAGE" "$(dirname "$OUTPUT")"

if command -v git >/dev/null 2>&1 && git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git -C "$ROOT" archive --format=tar HEAD | tar -xf - -C "$STAGE"
else
  rsync -a "$ROOT"/ "$STAGE"/ \
    --exclude '.git/' \
    --exclude 'target/' \
    --exclude '.agents_tmp/' \
    --exclude '__MACOSX/' \
    --exclude '.cargo-home/' \
    --exclude '.cargo-target/' \
    --exclude '*.ext4' \
    --exclude '*.img' \
    --exclude '*.qcow2' \
    --exclude '*.log' \
    --exclude 'dist/'
fi

find "$STAGE" -name '__MACOSX' -prune -exec rm -rf {} +
find "$STAGE" -name 'target' -prune -exec rm -rf {} +
find "$STAGE" -name '.agents_tmp' -prune -exec rm -rf {} +
find "$STAGE" -name '.cargo-home' -prune -exec rm -rf {} +
find "$STAGE" -name '.cargo-target' -prune -exec rm -rf {} +
find "$STAGE" \( -name '*.ext4' -o -name '*.img' -o -name '*.qcow2' -o -name '*.log' \) -delete

tar -czf "$OUTPUT" -C "$STAGE" .
echo "wrote source release bundle: $OUTPUT"
