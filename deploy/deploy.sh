#!/usr/bin/env bash
set -euo pipefail

SERVERS="${SERVERS:-}"
ZEROBOOT_BIN="${ZEROBOOT_BIN:-target/release/zeroboot}"
KERNEL="${KERNEL:-guest/vmlinux-fc}"
ROOTFS_PYTHON="${ROOTFS_PYTHON:-guest/rootfs-python.ext4}"
ROOTFS_NODE="${ROOTFS_NODE:-guest/rootfs-node.ext4}"
API_KEYS_FILE="${API_KEYS_FILE:-api_keys.json}"
PORT="${PORT:-8080}"
REMOTE_ROOT="${REMOTE_ROOT:-/var/lib/zeroboot}"
PY_TEMPLATE_DIR="$REMOTE_ROOT/templates/python"
NODE_TEMPLATE_DIR="$REMOTE_ROOT/templates/node"

[[ -n "$SERVERS" ]] || { echo "set SERVERS='host1 host2'" >&2; exit 1; }
[[ -f "$API_KEYS_FILE" ]] || { echo "missing API keys file: $API_KEYS_FILE" >&2; exit 1; }
[[ -f "$KERNEL" && -f "$ROOTFS_PYTHON" && -f "$ROOTFS_NODE" ]] || { echo 'missing kernel/rootfs artifact' >&2; exit 1; }

if [[ ! -x "$ZEROBOOT_BIN" ]]; then
  cargo build --release
fi

for server in $SERVERS; do
  echo "=== $server ==="
  ssh "$server" "sudo mkdir -p $PY_TEMPLATE_DIR $NODE_TEMPLATE_DIR /etc/zeroboot /var/log/zeroboot && sudo chown -R zeroboot:zeroboot $REMOTE_ROOT /etc/zeroboot /var/log/zeroboot"
  scp "$ZEROBOOT_BIN" "$server:/tmp/zeroboot"
  scp "$KERNEL" "$server:/tmp/vmlinux-fc"
  scp "$ROOTFS_PYTHON" "$server:/tmp/rootfs-python.ext4"
  scp "$ROOTFS_NODE" "$server:/tmp/rootfs-node.ext4"
  scp "$API_KEYS_FILE" "$server:/tmp/api_keys.json"
  scp deploy/zeroboot.service "$server:/tmp/zeroboot.service"
  ssh "$server" "sudo mv /tmp/zeroboot /usr/local/bin/zeroboot && sudo chmod +x /usr/local/bin/zeroboot &&     sudo mv /tmp/vmlinux-fc $REMOTE_ROOT/vmlinux-fc &&     sudo mv /tmp/rootfs-python.ext4 $REMOTE_ROOT/rootfs-python.ext4 &&     sudo mv /tmp/rootfs-node.ext4 $REMOTE_ROOT/rootfs-node.ext4 &&     sudo mv /tmp/api_keys.json /etc/zeroboot/api_keys.json && sudo chmod 0600 /etc/zeroboot/api_keys.json &&     sudo mv /tmp/zeroboot.service /etc/systemd/system/zeroboot.service"
  ssh "$server" "cd $REMOTE_ROOT && sudo /usr/local/bin/zeroboot template $REMOTE_ROOT/vmlinux-fc $REMOTE_ROOT/rootfs-python.ext4 $PY_TEMPLATE_DIR 20 /init 512"
  ssh "$server" "cd $REMOTE_ROOT && sudo /usr/local/bin/zeroboot template $REMOTE_ROOT/vmlinux-fc $REMOTE_ROOT/rootfs-node.ext4 $NODE_TEMPLATE_DIR 20 /init 512"
  ssh "$server" "sudo systemctl daemon-reload && sudo systemctl enable zeroboot && sudo systemctl restart zeroboot"
  ssh "$server" "curl -fsS http://127.0.0.1:$PORT/v1/ready"
done
