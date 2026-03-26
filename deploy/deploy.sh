#!/usr/bin/env bash
set -euo pipefail

# Versioned deployment script with rollback support
# Creates immutable releases and switches symlinks for atomic deployment

SERVERS="${SERVERS:-}"
ZEROBOOT_BIN="${ZEROBOOT_BIN:-target/release/zeroboot}"
KERNEL="${KERNEL:-guest/vmlinux-fc}"
ROOTFS_PYTHON="${ROOTFS_PYTHON:-guest/rootfs-python.ext4}"
ROOTFS_NODE="${ROOTFS_NODE:-guest/rootfs-node.ext4}"
API_KEYS_FILE="${API_KEYS_FILE:-api_keys.json}"
PEPPER_FILE="${PEPPER_FILE:-pepper.txt}"
PORT="${PORT:-8080}"
REMOTE_ROOT="${REMOTE_ROOT:-/var/lib/zeroboot}"
AUTH_MODE="${AUTH_MODE:-dev}"
REQUIRE_TEMPLATE_HASHES="${REQUIRE_TEMPLATE_HASHES:-false}"
REQUIRE_TEMPLATE_SIGNATURES="${REQUIRE_TEMPLATE_SIGNATURES:-false}"
KEYRING_FILE="${KEYRING_FILE:-keyring.json}"
PY_TEMPLATE_DIR="$REMOTE_ROOT/templates/python"
NODE_TEMPLATE_DIR="$REMOTE_ROOT/templates/node"

# Release directory structure:
# /var/lib/zeroboot/
#   releases/
#     <release_id>/
#       bin/zeroboot
#       vmlinux-fc
#       rootfs-python.ext4
#       rootfs-node.ext4
#       templates/python/
#       templates/node/
#   current -> releases/<release_id>/  (symlink)

[[ -n "$SERVERS" ]] || { echo "set SERVERS='host1 host2'" >&2; exit 1; }
[[ -f "$API_KEYS_FILE" ]] || { echo "missing API keys file: $API_KEYS_FILE" >&2; exit 1; }
[[ -f "$PEPPER_FILE" ]] || { echo "missing pepper file: $PEPPER_FILE" >&2; exit 1; }
[[ -f "$KERNEL" && -f "$ROOTFS_PYTHON" && -f "$ROOTFS_NODE" ]] || { echo 'missing kernel/rootfs artifact' >&2; exit 1; }

# Build binary if needed
if [[ ! -x "$ZEROBOOT_BIN" ]]; then
  cargo build --release
fi

# Generate unique release ID
RELEASE_ID="rel-$(date +%Y%m%d-%H%M%S)-$(head -c 4 /dev/urandom | xxd -p)"

for server in $SERVERS; do
  echo "=== Deploying to $server (release: $RELEASE_ID) ==="
  
  # Create release directory
  ssh "$server" "sudo mkdir -p $REMOTE_ROOT/releases/$RELEASE_ID/bin $REMOTE_ROOT/releases/$RELEASE_ID/templates/python $REMOTE_ROOT/releases/$RELEASE_ID/templates/node /etc/zeroboot /var/log/zeroboot && sudo chown -R zeroboot:zeroboot $REMOTE_ROOT/releases"
  
  # Upload binary and artifacts to release directory
  scp "$ZEROBOOT_BIN" "$server:/tmp/zeroboot"
  scp "$KERNEL" "$server:/tmp/vmlinux-fc"
  scp "$ROOTFS_PYTHON" "$server:/tmp/rootfs-python.ext4"
  scp "$ROOTFS_NODE" "$server:/tmp/rootfs-node.ext4"
  scp "$API_KEYS_FILE" "$server:/tmp/api_keys.json"
  scp "$PEPPER_FILE" "$server:/tmp/pepper"
  if [[ -f "$KEYRING_FILE" ]]; then
    scp "$KEYRING_FILE" "$server:/tmp/keyring.json"
  fi
  
  # Use the pre-configured service file - no sed patching needed
  scp deploy/zeroboot.service "$server:/tmp/zeroboot.service"
  
  # Move to release directory
  ssh "$server" "sudo mv /tmp/zeroboot $REMOTE_ROOT/releases/$RELEASE_ID/bin/zeroboot && sudo chmod +x $REMOTE_ROOT/releases/$RELEASE_ID/bin/zeroboot"
  ssh "$server" "sudo mv /tmp/vmlinux-fc $REMOTE_ROOT/releases/$RELEASE_ID/vmlinux-fc"
  ssh "$server" "sudo mv /tmp/rootfs-python.ext4 $REMOTE_ROOT/releases/$RELEASE_ID/rootfs-python.ext4"
  ssh "$server" "sudo mv /tmp/rootfs-node.ext4 $REMOTE_ROOT/releases/$RELEASE_ID/rootfs-node.ext4"
  ssh "$server" "sudo mv /tmp/api_keys.json /etc/zeroboot/api_keys.json && sudo chmod 0600 /etc/zeroboot/api_keys.json"
  ssh "$server" "sudo mv /tmp/pepper /etc/zeroboot/pepper && sudo chmod 0600 /etc/zeroboot/pepper"
  if [[ -f "$KEYRING_FILE" ]]; then
    ssh "$server" "sudo mv /tmp/keyring.json /etc/zeroboot/keyring.json && sudo chmod 0600 /etc/zeroboot/keyring.json"
  fi
  
  # Generate templates using the new release
  ssh "$server" "cd $REMOTE_ROOT/releases/$RELEASE_ID && sudo ZEROBOOT_AUTH_MODE=$AUTH_MODE ZEROBOOT_REQUIRE_TEMPLATE_HASHES=$REQUIRE_TEMPLATE_HASHES ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=$REQUIRE_TEMPLATE_SIGNATURES $REMOTE_ROOT/releases/$RELEASE_ID/bin/zeroboot template $REMOTE_ROOT/releases/$RELEASE_ID/vmlinux-fc $REMOTE_ROOT/releases/$RELEASE_ID/rootfs-python.ext4 $REMOTE_ROOT/releases/$RELEASE_ID/templates/python 20 /init 512"
  ssh "$server" "cd $REMOTE_ROOT/releases/$RELEASE_ID && sudo ZEROBOOT_AUTH_MODE=$AUTH_MODE ZEROBOOT_REQUIRE_TEMPLATE_HASHES=$REQUIRE_TEMPLATE_HASHES ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=$REQUIRE_TEMPLATE_SIGNATURES $REMOTE_ROOT/releases/$RELEASE_ID/bin/zeroboot template $REMOTE_ROOT/releases/$RELEASE_ID/vmlinux-fc $REMOTE_ROOT/releases/$RELEASE_ID/rootfs-node.ext4 $REMOTE_ROOT/releases/$RELEASE_ID/templates/node 20 /init 512"
  
  # Run smoke test before switching
  echo "Running smoke test..."
  ssh "$server" "sudo ZEROBOOT_AUTH_MODE=$AUTH_MODE ZEROBOOT_REQUIRE_TEMPLATE_HASHES=$REQUIRE_TEMPLATE_HASHES ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=$REQUIRE_TEMPLATE_SIGNATURES $REMOTE_ROOT/releases/$RELEASE_ID/bin/zeroboot test-exec $REMOTE_ROOT/releases/$RELEASE_ID/templates/python python 'print(1+1)'" || {
    echo "SMOKE TEST FAILED - not switching to new release"
    ssh "$server" "sudo rm -rf $REMOTE_ROOT/releases/$RELEASE_ID"
    continue
  }
  
  # Switch symlink atomically
  echo "Switching to new release..."
  ssh "$server" "cd $REMOTE_ROOT && sudo ln -sfn releases/$RELEASE_ID current"
  
  # Install or update service (no sed patching needed - service uses symlink path)
  ssh "$server" "sudo mv /tmp/zeroboot.service /etc/systemd/system/zeroboot.service"
  ssh "$server" "sudo systemctl daemon-reload"
  
  # Set environment for systemd service via drop-in or environment file
  ssh "$server" "echo 'ZEROBOOT_AUTH_MODE=$AUTH_MODE' | sudo tee /etc/zeroboot/env"
  ssh "$server" "echo 'ZEROBOOT_REQUIRE_TEMPLATE_HASHES=$REQUIRE_TEMPLATE_HASHES' | sudo tee -a /etc/zeroboot/env"
  ssh "$server" "echo 'ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=$REQUIRE_TEMPLATE_SIGNATURES' | sudo tee -a /etc/zeroboot/env"
  
  # Restart service
  ssh "$server" "sudo systemctl restart zeroboot"
  
  # Verify health
  sleep 3
  ssh "$server" "curl -fsS http://127.0.0.1:$PORT/v1/ready" || {
    echo "HEALTH CHECK FAILED - rolling back..."
    # Get previous release
    PREV_RELEASE=$(ssh "$server" "ls -1 $REMOTE_ROOT/releases/ | grep -v '^$RELEASE_ID$' | tail -1")
    if [[ -n "$PREV_RELEASE" ]]; then
      echo "Rolling back to $PREV_RELEASE"
      ssh "$server" "cd $REMOTE_ROOT && sudo ln -sfn releases/$PREV_RELEASE current"
      ssh "$server" "sudo systemctl restart zeroboot"
      sleep 2
      ssh "$server" "curl -fsS http://127.0.0.1:$PORT/v1/ready" || {
        echo "ROLLBACK FAILED - manual intervention required!"
        exit 1
      }
      echo "Rollback successful"
    else
      echo "No previous release to rollback to!"
      exit 1
    fi
    continue
  }
  
  echo "Deployment to $server completed successfully"
done

echo "=== All deployments complete ==="
