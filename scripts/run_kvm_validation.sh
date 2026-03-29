#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
usage: scripts/run_kvm_validation.sh [--work-root <dir>] [--port <port>] [--repeat <count>] [--skip-host-check]

Runs the Linux x86_64 KVM validation flow for XBOOT:
  1. Host readiness checks
  2. Release build
  3. Pinned artifact fetch
  4. Rootfs/template assembly
  5. verify-startup + test-exec
  6. HTTP smoke + repeated smoke
  7. Current/previous release layout verification
EOF
}

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_ROOT="/tmp/xboot-kvm-validation"
PORT="8080"
REPEAT_COUNT="100"
SKIP_HOST_CHECK="0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --work-root)
      WORK_ROOT="${2:-}"
      shift 2
      ;;
    --port)
      PORT="${2:-}"
      shift 2
      ;;
    --repeat)
      REPEAT_COUNT="${2:-}"
      shift 2
      ;;
    --skip-host-check)
      SKIP_HOST_CHECK="1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

[[ -n "$WORK_ROOT" ]] || { echo "--work-root cannot be empty" >&2; exit 1; }
[[ "$PORT" =~ ^[0-9]+$ ]] || { echo "--port must be numeric" >&2; exit 1; }
[[ "$REPEAT_COUNT" =~ ^[0-9]+$ ]] || { echo "--repeat must be numeric" >&2; exit 1; }

need() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing required command: $1" >&2; exit 1; }
}

ensure_dir() {
  local dir="$1"
  if [[ "$(id -u)" -eq 0 ]]; then
    mkdir -p "$dir"
  else
    sudo mkdir -p "$dir"
  fi
}

cleanup_server() {
  if [[ -f "$PID_FILE" ]]; then
    local pid
    pid="$(cat "$PID_FILE" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  fi
}

wait_for_ready() {
  local attempt
  for attempt in $(seq 1 30); do
    if curl -fsS "http://127.0.0.1:$PORT/ready" >/dev/null 2>&1; then
      return 0
    fi
    if [[ -f "$PID_FILE" ]]; then
      local pid
      pid="$(cat "$PID_FILE" 2>/dev/null || true)"
      if [[ -n "$pid" ]] && ! kill -0 "$pid" 2>/dev/null; then
        echo "zeroboot exited before becoming ready" >&2
        cat "$LOG_FILE" >&2 || true
        return 1
      fi
    fi
    sleep 1
  done
  echo "zeroboot did not become ready on port $PORT" >&2
  cat "$LOG_FILE" >&2 || true
  return 1
}

write_deploy_state() {
  local path="$1"
  local current_release="$2"
  local previous_release="$3"
  python3 - "$path" "$current_release" "$previous_release" <<'PY'
import json
import pathlib
import sys
import time

path = pathlib.Path(sys.argv[1])
previous = sys.argv[3]
state = {
    "current_release": sys.argv[2],
    "previous_release": None if previous == "null" else previous,
    "updated_at_unix_ms": int(time.time() * 1000),
}
path.write_text(json.dumps(state, indent=2) + "\n")
PY
}

make_manual_exec_request() {
  local language="$1"
  local code="$2"
  curl -fsS -X POST "http://127.0.0.1:$PORT/v1/exec" \
    -H 'content-type: application/json' \
    -H "authorization: Bearer $SMOKE_TOKEN" \
    -d "{\"language\":\"$language\",\"code\":\"$code\",\"timeout_seconds\":5}"
}

need cargo
need curl
need jq
need mkfs.ext4
need python3
need sha256sum
need tar

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "run_kvm_validation.sh requires Linux" >&2
  exit 1
fi

if [[ "$(uname -m)" != "x86_64" ]]; then
  echo "run_kvm_validation.sh requires x86_64" >&2
  exit 1
fi

ensure_dir /var/lib/zeroboot
ensure_dir /etc/zeroboot
ensure_dir /opt/xboot

if [[ "$SKIP_HOST_CHECK" != "1" ]]; then
  bash "$ROOT/scripts/check_kvm_host.sh"
  bash "$ROOT/scripts/preflight.sh"
fi

ARTIFACTS_DIR="$WORK_ROOT/artifacts"
BASE_TEMPLATE_DIR="$WORK_ROOT/rootfs-base"
PY_TEMPLATE_DIR="$WORK_ROOT/rootfs-python-template"
NODE_TEMPLATE_DIR="$WORK_ROOT/rootfs-node-template"
PY_STAGING_DIR="$WORK_ROOT/staging-python"
NODE_STAGING_DIR="$WORK_ROOT/staging-node"
PY_ROOTFS="$WORK_ROOT/rootfs-python.ext4"
NODE_ROOTFS="$WORK_ROOT/rootfs-node.ext4"
RELEASE_DIR="$WORK_ROOT/release"
DEPLOY_ROOT="$WORK_ROOT/deploy-root"
SIGNING_KEY="$WORK_ROOT/signing-key.pkcs8"
KEYGEN_TXT="$WORK_ROOT/keygen.txt"
KEYRING_FILE="$WORK_ROOT/keyring.json"
PEPPER_FILE="$WORK_ROOT/pepper.txt"
API_KEYS_FILE="$WORK_ROOT/api-keys.json"
API_KEYGEN_TXT="$WORK_ROOT/api-keygen.txt"
LOG_FILE="$WORK_ROOT/zeroboot.log"
PID_FILE="$WORK_ROOT/zeroboot.pid"

rm -rf "$WORK_ROOT"
mkdir -p "$WORK_ROOT"

trap cleanup_server EXIT

echo "=== XBOOT Linux/KVM Validation ==="
echo "Repo root: $ROOT"
echo "Work root: $WORK_ROOT"
echo "Port: $PORT"
echo "Repeat count: $REPEAT_COUNT"
echo ""

echo "[1/8] Building release binary..."
(cd "$ROOT" && cargo build --locked --release)

echo "[2/8] Fetching pinned artifacts..."
bash "$ROOT/scripts/fetch_official_artifacts.sh" "$ARTIFACTS_DIR"
FC_DIR="$ARTIFACTS_DIR/firecracker/release-v1.12.0-x86_64"
FC_BIN="$FC_DIR/firecracker-v1.12.0-x86_64"
export ZEROBOOT_FIRECRACKER_BIN="$FC_BIN"
export PATH="$FC_DIR:$PATH"

echo "[3/8] Preparing rootfs templates..."
bash "$ROOT/scripts/prepare_rootfs_template.sh" "$ARTIFACTS_DIR/rootfs/ubuntu-22.04.ext4" "$BASE_TEMPLATE_DIR"
cp -a "$BASE_TEMPLATE_DIR" "$PY_TEMPLATE_DIR"
cp -a "$BASE_TEMPLATE_DIR" "$NODE_TEMPLATE_DIR"
bash "$ROOT/scripts/install_node_runtime.sh" "$NODE_TEMPLATE_DIR" "$ARTIFACTS_DIR"

echo "[4/8] Building guest rootfs images..."
bash "$ROOT/scripts/build_guest_rootfs.sh" python "$PY_STAGING_DIR" --rootfs-template "$PY_TEMPLATE_DIR"
bash "$ROOT/scripts/build_rootfs_image.sh" "$PY_STAGING_DIR" "$PY_ROOTFS"
bash "$ROOT/scripts/build_guest_rootfs.sh" node "$NODE_STAGING_DIR" --rootfs-template "$NODE_TEMPLATE_DIR"
bash "$ROOT/scripts/build_rootfs_image.sh" "$NODE_STAGING_DIR" "$NODE_ROOTFS"

echo "[5/8] Assembling promoted release..."
mkdir -p "$RELEASE_DIR/bin" "$RELEASE_DIR/templates/python" "$RELEASE_DIR/templates/node"
cp "$ROOT/target/release/zeroboot" "$RELEASE_DIR/bin/zeroboot"
"$RELEASE_DIR/bin/zeroboot" template "$ARTIFACTS_DIR/kernel/vmlinux-5.10.223" "$PY_ROOTFS" "$RELEASE_DIR/templates/python" 20 /init 512
"$RELEASE_DIR/bin/zeroboot" template "$ARTIFACTS_DIR/kernel/vmlinux-5.10.223" "$NODE_ROOTFS" "$RELEASE_DIR/templates/node" 20 /init 512
"$RELEASE_DIR/bin/zeroboot" keygen "$SIGNING_KEY" > "$KEYGEN_TXT"
KEY_ID="$(awk '/^Key ID:/ {print $3; exit}' "$KEYGEN_TXT")"
PUBLIC_KEY="$(awk '/^Public Key \(base64\):/{getline; gsub(/^  /,""); print; exit}' "$KEYGEN_TXT")"
python3 - "$KEY_ID" "$PUBLIC_KEY" "$KEYRING_FILE" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[3])
payload = {
    "keys": [
        {
            "key_id": sys.argv[1],
            "algorithm": "ed25519",
            "public_key": sys.argv[2],
            "enabled": True,
            "description": "local validation signer",
        }
    ]
}
path.write_text(json.dumps(payload, indent=2) + "\n")
PY
"$RELEASE_DIR/bin/zeroboot" promote-template "$RELEASE_DIR/templates/python/template.manifest.json" --channel prod --key "$SIGNING_KEY" --key-id "$KEY_ID" --receipt "$RELEASE_DIR/templates/python/promotion-receipt.json"
"$RELEASE_DIR/bin/zeroboot" promote-template "$RELEASE_DIR/templates/node/template.manifest.json" --channel prod --key "$SIGNING_KEY" --key-id "$KEY_ID" --receipt "$RELEASE_DIR/templates/node/promotion-receipt.json"
python3 "$ROOT/scripts/create_release_receipt.py" "$RELEASE_DIR" kvm-validation >/dev/null
printf 'validation-pepper\n' > "$PEPPER_FILE"
python3 "$ROOT/scripts/make_api_keys.py" --count 1 --pepper-file "$PEPPER_FILE" --output "$API_KEYS_FILE" > "$API_KEYGEN_TXT"
SMOKE_TOKEN="$(awk '/^zb_live_/{print; exit}' "$API_KEYGEN_TXT")"
[[ -n "$SMOKE_TOKEN" ]] || { echo "failed to generate smoke token" >&2; exit 1; }

FC_SHA="$(sha256sum "$FC_BIN" | awk '{print $1}')"
FC_VERSION="$(firecracker --version 2>&1)"
export ZEROBOOT_AUTH_MODE=prod
export ZEROBOOT_API_KEYS_FILE="$API_KEYS_FILE"
export ZEROBOOT_API_KEY_PEPPER_FILE="$PEPPER_FILE"
export ZEROBOOT_REQUIRE_TEMPLATE_HASHES=true
export ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=true
export ZEROBOOT_KEYRING_PATH="$KEYRING_FILE"
export ZEROBOOT_ALLOWED_FIRECRACKER_VERSION="$FC_VERSION"
export ZEROBOOT_ALLOWED_FC_BINARY_SHA256="$FC_SHA"
export ZEROBOOT_RELEASE_CHANNEL=prod
export ZEROBOOT_REQUEST_LOG_PATH="$WORK_ROOT/requests.jsonl"
export ZEROBOOT_LOG_CODE=false

echo "[6/8] Running verify-startup and test-exec..."
"$RELEASE_DIR/bin/zeroboot" verify-startup "python:$RELEASE_DIR/templates/python,node:$RELEASE_DIR/templates/node" --release-root "$RELEASE_DIR"
"$RELEASE_DIR/bin/zeroboot" test-exec "$RELEASE_DIR/templates/python" python "print(1+1)"
"$RELEASE_DIR/bin/zeroboot" test-exec "$RELEASE_DIR/templates/node" node "console.log(1+1)"

echo "[7/8] Starting API and running smoke tests..."
"$RELEASE_DIR/bin/zeroboot" serve "python:$RELEASE_DIR/templates/python,node:$RELEASE_DIR/templates/node" "$PORT" > "$LOG_FILE" 2>&1 &
echo $! > "$PID_FILE"
wait_for_ready
curl -fsS "http://127.0.0.1:$PORT/live" >/dev/null
curl -fsS "http://127.0.0.1:$PORT/ready" >/dev/null
curl -fsS "http://127.0.0.1:$PORT/health" >/dev/null
make_manual_exec_request python 'print(40+2)' | jq -e '.stdout == "42"' >/dev/null
make_manual_exec_request node 'console.log(40+2)' | jq -e '.stdout == "42"' >/dev/null
bash "$ROOT/scripts/smoke_exec.sh" "$SMOKE_TOKEN" "http://127.0.0.1:$PORT"
bash "$ROOT/scripts/repeat_smoke.sh" "$SMOKE_TOKEN" "http://127.0.0.1:$PORT" "$REPEAT_COUNT"

echo "[8/8] Verifying current/previous release layout..."
rm -rf "$DEPLOY_ROOT"
mkdir -p "$DEPLOY_ROOT/releases"
cp -a "$RELEASE_DIR" "$DEPLOY_ROOT/releases/release-a"
cp -a "$RELEASE_DIR" "$DEPLOY_ROOT/releases/release-b"
python3 "$ROOT/scripts/create_release_receipt.py" "$DEPLOY_ROOT/releases/release-a" release-a >/dev/null
python3 "$ROOT/scripts/create_release_receipt.py" "$DEPLOY_ROOT/releases/release-b" release-b >/dev/null
ln -sfn "$DEPLOY_ROOT/releases/release-a" "$DEPLOY_ROOT/current"
write_deploy_state "$DEPLOY_ROOT/deploy-state.json" release-a null
"$RELEASE_DIR/bin/zeroboot" verify-startup "python:$DEPLOY_ROOT/current/templates/python,node:$DEPLOY_ROOT/current/templates/node" --release-root "$DEPLOY_ROOT/current"
ln -sfn "$DEPLOY_ROOT/releases/release-b" "$DEPLOY_ROOT/current"
write_deploy_state "$DEPLOY_ROOT/deploy-state.json" release-b release-a
"$RELEASE_DIR/bin/zeroboot" verify-startup "python:$DEPLOY_ROOT/current/templates/python,node:$DEPLOY_ROOT/current/templates/node" --release-root "$DEPLOY_ROOT/current"
ln -sfn "$DEPLOY_ROOT/releases/release-a" "$DEPLOY_ROOT/current"
"$RELEASE_DIR/bin/zeroboot" verify-startup "python:$DEPLOY_ROOT/current/templates/python,node:$DEPLOY_ROOT/current/templates/node" --release-root "$DEPLOY_ROOT/current"

echo ""
echo "Validation completed successfully."
echo "Smoke token: $SMOKE_TOKEN"
