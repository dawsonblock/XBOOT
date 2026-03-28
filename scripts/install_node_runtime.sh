#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF' >&2
usage: scripts/install_node_runtime.sh <rootfs-template-dir> <artifacts-dir>

Install the pinned Node.js runtime into a rootfs template directory so that
`build_guest_rootfs.sh node --rootfs-template ...` produces a guest image with
`/usr/bin/node` present.
EOF
}

ROOTFS_TEMPLATE="${1:-}"
ARTIFACTS_DIR="${2:-}"
[[ -n "$ROOTFS_TEMPLATE" && -n "$ARTIFACTS_DIR" ]] || { usage; exit 1; }
[[ -d "$ROOTFS_TEMPLATE" ]] || { echo "rootfs template is not a directory: $ROOTFS_TEMPLATE" >&2; exit 1; }

NODE_VERSION="20.20.2"
NODE_SHA256="df770b2a6f130ed8627c9782c988fda9669fa23898329a61a871e32f965e007d"
NODE_TARBALL="$ARTIFACTS_DIR/node/node-v20.20.2-linux-x64.tar.xz"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

command -v tar >/dev/null 2>&1 || { echo "tar is required" >&2; exit 1; }
[[ -f "$NODE_TARBALL" ]] || { echo "missing node tarball: $NODE_TARBALL" >&2; exit 1; }

actual_sha="$(sha256_file "$NODE_TARBALL")"
[[ "$actual_sha" == "$NODE_SHA256" ]] || {
  echo "node tarball sha256 mismatch: expected $NODE_SHA256 got $actual_sha" >&2
  exit 1
}

install_root="$ROOTFS_TEMPLATE/usr/local/node-v$NODE_VERSION"
rm -rf "$install_root"
mkdir -p "$ROOTFS_TEMPLATE/usr/local" "$ROOTFS_TEMPLATE/usr/bin"
tar -xJf "$NODE_TARBALL" -C "$ROOTFS_TEMPLATE/usr/local"
mv "$ROOTFS_TEMPLATE/usr/local/node-v$NODE_VERSION-linux-x64" "$install_root"

ln -sfn "../local/node-v$NODE_VERSION/bin/node" "$ROOTFS_TEMPLATE/usr/bin/node"
ln -sfn "../local/node-v$NODE_VERSION/bin/npm" "$ROOTFS_TEMPLATE/usr/bin/npm"
ln -sfn "../local/node-v$NODE_VERSION/bin/npx" "$ROOTFS_TEMPLATE/usr/bin/npx"

cat <<EOF
Installed Node.js v$NODE_VERSION into $ROOTFS_TEMPLATE
  binary: $ROOTFS_TEMPLATE/usr/bin/node
  npm:    $ROOTFS_TEMPLATE/usr/bin/npm
  npx:    $ROOTFS_TEMPLATE/usr/bin/npx
EOF
