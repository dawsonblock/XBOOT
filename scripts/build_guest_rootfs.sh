#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<EOF >&2
usage: $0 <python|node> <staging-dir> [--rootfs-template <dir>] [--write-manifest <path>]

This script does not fabricate kernel or distro artifacts.
It builds a deterministic staging tree from a caller-supplied rootfs template,
copies the guest assets into place, compiles guest/init.c when a C compiler is
available, and writes a manifest with file hashes.
EOF
}

LANGUAGE="${1:-}"
STAGING_DIR="${2:-}"
shift 2 || true
[[ -n "$LANGUAGE" && -n "$STAGING_DIR" ]] || { usage; exit 1; }
case "$LANGUAGE" in python|node) ;; *) echo "unsupported language: $LANGUAGE" >&2; exit 1 ;; esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/manifests/${LANGUAGE}-guest.manifest"
[[ -f "$MANIFEST" ]] || { echo "missing manifest: $MANIFEST" >&2; exit 1; }

ROOTFS_TEMPLATE=""
OUT_MANIFEST=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --rootfs-template) ROOTFS_TEMPLATE="${2:-}"; shift 2 ;;
    --write-manifest) OUT_MANIFEST="${2:-}"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 1 ;;
  esac
done

required_unset=$(grep '=REQUIRED$' "$MANIFEST" || true)
if [[ -n "$required_unset" ]]; then
  echo "manifest still contains REQUIRED values:" >&2
  echo "$required_unset" >&2
  exit 1
fi

rm -rf "$STAGING_DIR"
mkdir -p "$STAGING_DIR"/zeroboot "$STAGING_DIR"/usr/bin "$STAGING_DIR"/tmp
if [[ -n "$ROOTFS_TEMPLATE" ]]; then
  [[ -d "$ROOTFS_TEMPLATE" ]] || { echo "rootfs template is not a directory: $ROOTFS_TEMPLATE" >&2; exit 1; }
  cp -a "$ROOTFS_TEMPLATE"/. "$STAGING_DIR"/
fi

cp "$ROOT"/guest/*.py "$STAGING_DIR/zeroboot/"
cp "$ROOT"/guest/*.js "$STAGING_DIR/zeroboot/"

if command -v gcc >/dev/null 2>&1; then
  gcc -Os -static -s -o "$STAGING_DIR/init" "$ROOT/guest/init.c"
else
  echo "warning: gcc not found; guest init binary not built" >&2
fi

if [[ -x "$STAGING_DIR/init" ]]; then
  chmod +x "$STAGING_DIR/init"
fi
chmod 0644 "$STAGING_DIR/zeroboot/worker.py" "$STAGING_DIR/zeroboot/worker_node.js"
chmod 0644 "$STAGING_DIR"/zeroboot/*.py "$STAGING_DIR"/zeroboot/*.js

if [[ -n "$OUT_MANIFEST" ]]; then
  {
    echo "language=$LANGUAGE"
    echo "generated_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "staging_dir=$STAGING_DIR"
    if [[ -f "$STAGING_DIR/init" ]]; then sha256sum "$STAGING_DIR/init" | awk '{print "init_sha256=" $1}'; fi
    sha256sum "$STAGING_DIR/zeroboot/worker_supervisor.py" | awk '{print "worker_py_sha256=" $1}'
    sha256sum "$STAGING_DIR/zeroboot/worker_supervisor.js" | awk '{print "worker_node_sha256=" $1}'
    find "$STAGING_DIR" -type f -printf '%P
' | sort | while read -r rel; do
      sha256sum "$STAGING_DIR/$rel" | awk -v rel="$rel" '{print "file=" rel " sha256=" $1}'
    done
  } > "$OUT_MANIFEST"
  echo "wrote manifest: $OUT_MANIFEST"
fi

echo "prepared guest staging tree at $STAGING_DIR"
