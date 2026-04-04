#!/usr/bin/env bash
set -euo pipefail

# Immutable release deployment script.
#
# Expected local release directory layout:
#   <release>/
#     bin/zeroboot
#     templates/python/
#     templates/node/
#     release-receipt.json
#
# release-receipt.json schema (minimum):
# {
#   "release_id": "rel-20260328-123456",
#   "templates": [
#     {"language":"python","workdir":"templates/python","manifest_path":"templates/python/template.manifest.json"},
#     {"language":"node","workdir":"templates/node","manifest_path":"templates/node/template.manifest.json"}
#   ]
# }

SERVERS="${SERVERS:-}"
RELEASE_DIR="${RELEASE_DIR:-}"
PORT="${PORT:-8080}"
REMOTE_ROOT="${REMOTE_ROOT:-/var/lib/zeroboot}"
AUTH_MODE="${AUTH_MODE:-dev}"
API_KEYS_FILE="${API_KEYS_FILE:-api_keys.json}"
PEPPER_FILE="${PEPPER_FILE:-pepper.txt}"
KEYRING_FILE="${KEYRING_FILE:-keyring.json}"
REQUIRE_TEMPLATE_HASHES="${REQUIRE_TEMPLATE_HASHES:-false}"
REQUIRE_TEMPLATE_SIGNATURES="${REQUIRE_TEMPLATE_SIGNATURES:-false}"
MIN_FREE_BYTES="${MIN_FREE_BYTES:-536870912}"
MIN_FREE_INODES="${MIN_FREE_INODES:-10000}"
SMOKE_BEARER_TOKEN="${SMOKE_BEARER_TOKEN:-}"

[[ -n "$SERVERS" ]] || { echo "set SERVERS='host1 host2'" >&2; exit 1; }
[[ -n "$RELEASE_DIR" ]] || { echo "set RELEASE_DIR=/path/to/prebuilt-release" >&2; exit 1; }
[[ -d "$RELEASE_DIR" ]] || { echo "missing release dir: $RELEASE_DIR" >&2; exit 1; }
[[ -x "$RELEASE_DIR/bin/zeroboot" ]] || { echo "release missing executable bin/zeroboot" >&2; exit 1; }
[[ -f "$RELEASE_DIR/release-receipt.json" ]] || { echo "release missing release-receipt.json" >&2; exit 1; }
[[ -f "$API_KEYS_FILE" ]] || { echo "missing API keys file: $API_KEYS_FILE" >&2; exit 1; }
[[ -f "$PEPPER_FILE" ]] || { echo "missing pepper file: $PEPPER_FILE" >&2; exit 1; }
if [[ "$AUTH_MODE" == "prod" && -z "$SMOKE_BEARER_TOKEN" ]]; then
  echo "prod deploy requires SMOKE_BEARER_TOKEN for /v1/exec smoke validation" >&2
  exit 1
fi
if [[ "$REQUIRE_TEMPLATE_SIGNATURES" == "true" && ! -f "$KEYRING_FILE" ]]; then
  echo "template signatures enabled but keyring file is missing: $KEYRING_FILE" >&2
  exit 1
fi

mapfile -t RECEIPT_INFO < <(python3 - "$RELEASE_DIR/release-receipt.json" "$RELEASE_DIR" <<'PY'
import json
import pathlib
import sys

receipt_path = pathlib.Path(sys.argv[1])
release_dir = pathlib.Path(sys.argv[2])
receipt = json.loads(receipt_path.read_text())
release_id = receipt.get("release_id")
templates = receipt.get("templates")
if not release_id:
    raise SystemExit("release-receipt.json missing release_id")
if not isinstance(templates, list) or not templates:
    raise SystemExit("release-receipt.json missing templates[]")

print(release_id)
for item in templates:
    language = item.get("language")
    workdir = item.get("workdir")
    manifest_path = item.get("manifest_path")
    if not language or not workdir or not manifest_path:
        raise SystemExit("template entry requires language, workdir, and manifest_path")
    if not (release_dir / workdir).is_dir():
        raise SystemExit(f"missing template workdir in release: {workdir}")
    if not (release_dir / manifest_path).is_file():
        raise SystemExit(f"missing template manifest in release: {manifest_path}")
    print(f"{language}|{workdir}|{manifest_path}")
PY
)

RELEASE_ID="${RECEIPT_INFO[0]}"
TEMPLATE_ENTRIES=("${RECEIPT_INFO[@]:1}")
[[ ${#TEMPLATE_ENTRIES[@]} -gt 0 ]] || { echo "release receipt contains no templates" >&2; exit 1; }

build_template_spec() {
  local release_root="$1"
  local parts=()
  local entry lang workdir manifest_path
  for entry in "${TEMPLATE_ENTRIES[@]}"; do
    IFS='|' read -r lang workdir manifest_path <<<"$entry"
    parts+=("${lang}:${release_root}/${workdir}")
  done
  local joined=""
  local part
  for part in "${parts[@]}"; do
    if [[ -n "$joined" ]]; then
      joined+="," 
    fi
    joined+="$part"
  done
  printf '%s' "$joined"
}

write_env_file() {
  local env_file="$1"
  {
    echo "ZEROBOOT_AUTH_MODE=$AUTH_MODE"
    echo "ZEROBOOT_API_KEYS_FILE=/etc/zeroboot/api_keys.json"
    echo "ZEROBOOT_API_KEY_PEPPER_FILE=/etc/zeroboot/pepper"
    echo "ZEROBOOT_REQUIRE_TEMPLATE_HASHES=$REQUIRE_TEMPLATE_HASHES"
    echo "ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=$REQUIRE_TEMPLATE_SIGNATURES"
    echo "ZEROBOOT_MIN_FREE_BYTES=$MIN_FREE_BYTES"
    echo "ZEROBOOT_MIN_FREE_INODES=$MIN_FREE_INODES"
    echo "ZEROBOOT_REQUEST_LOG_PATH=/var/lib/zeroboot/requests.jsonl"
    echo "ZEROBOOT_LOG_CODE=false"
    if [[ -n "${ZEROBOOT_ALLOWED_FIRECRACKER_VERSION:-}" ]]; then
      echo "ZEROBOOT_ALLOWED_FIRECRACKER_VERSION=$ZEROBOOT_ALLOWED_FIRECRACKER_VERSION"
    fi
    if [[ -n "${ZEROBOOT_ALLOWED_FC_BINARY_SHA256:-}" ]]; then
      echo "ZEROBOOT_ALLOWED_FC_BINARY_SHA256=$ZEROBOOT_ALLOWED_FC_BINARY_SHA256"
    fi
    if [[ -n "${ZEROBOOT_RELEASE_CHANNEL:-}" ]]; then
      echo "ZEROBOOT_RELEASE_CHANNEL=$ZEROBOOT_RELEASE_CHANNEL"
    fi
    if [[ -f "$KEYRING_FILE" ]]; then
      echo "ZEROBOOT_KEYRING_PATH=/etc/zeroboot/keyring.json"
    fi
  } >"$env_file"
}

get_remote_current_release() {
  local server="$1"
  ssh "$server" "python3 - '$REMOTE_ROOT' <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
state_path = root / 'deploy-state.json'
if state_path.is_file():
    try:
        state = json.loads(state_path.read_text())
        current = state.get('current_release')
        if current:
            print(current)
            raise SystemExit(0)
    except Exception:
        pass
current_link = root / 'current'
if current_link.is_symlink():
    target = current_link.resolve()
    print(target.name)
PY" | tr -d '\r'
}

update_remote_deploy_state() {
  local server="$1"
  local current_release="$2"
  local previous_release="$3"
  ssh "$server" "python3 - '$REMOTE_ROOT/deploy-state.json' '$current_release' '$previous_release' <<'PY'
import json
import pathlib
import sys
import time

state_path = pathlib.Path(sys.argv[1])
current_release = sys.argv[2]
previous_release = sys.argv[3]
state_path.parent.mkdir(parents=True, exist_ok=True)
payload = {
    'current_release': current_release or None,
    'previous_release': previous_release or None,
    'updated_at_unix_ms': int(time.time() * 1000),
}
tmp_path = state_path.with_suffix('.tmp')
tmp_path.write_text(json.dumps(payload, indent=2) + '\n')
tmp_path.replace(state_path)
PY"
}

run_remote_verify_startup() {
  local server="$1"
  local release_root="$2"
  local template_spec="$3"
  ssh "$server" "sudo bash -lc 'set -a; source /etc/zeroboot/env; set +a; \"$release_root/bin/zeroboot\" verify-startup \"$template_spec\" --release-root \"$release_root\"'"
}

run_remote_ready_check() {
  local server="$1"
  ssh "$server" "curl -fsS http://127.0.0.1:$PORT/v1/ready >/dev/null"
}

run_remote_health_check() {
  local server="$1"
  ssh "$server" "python3 - <<'PY'
import json
import urllib.request

with urllib.request.urlopen('http://127.0.0.1:$PORT/v1/health') as response:
    payload = json.load(response)
if payload.get('status') != 'ok':
    raise SystemExit(f\"/v1/health not ok: {payload.get('status')}\")
bad = sorted(
    name for name, status in (payload.get('templates') or {}).items()
    if not status.get('ready')
)
if bad:
    raise SystemExit('unhealthy templates: ' + ', '.join(bad))
PY"
}

run_remote_exec_smoke_for_language() {
  local server="$1"
  local language="$2"
  local code
  if [[ "$language" == "node" ]]; then
    code='console.log(40+2)'
  else
    code='print(40+2)'
  fi
  ssh "$server" "curl -fsS -X POST http://127.0.0.1:$PORT/v1/exec \
    -H 'content-type: application/json' \
    ${SMOKE_BEARER_TOKEN:+-H 'authorization: Bearer $SMOKE_BEARER_TOKEN'} \
    -d '{\"language\":\"$language\",\"code\":\"$code\",\"timeout_seconds\":5}' >/dev/null"
}

run_remote_exec_smoke_all() {
  local server="$1"
  local entry lang workdir manifest_path
  for entry in "${TEMPLATE_ENTRIES[@]}"; do
    IFS='|' read -r lang workdir manifest_path <<<"$entry"
    run_remote_exec_smoke_for_language "$server" "$lang" || return 1
  done
}

rollback_remote_release() {
  local server="$1"
  local failed_release="$2"
  local previous_release="$3"
  [[ -n "$previous_release" ]] || { echo "no previous release recorded for rollback on $server" >&2; return 1; }

  echo "Rolling back $server to $previous_release"
  ssh "$server" "cd '$REMOTE_ROOT' && sudo ln -sfn 'releases/$previous_release' current"
  update_remote_deploy_state "$server" "$previous_release" "$failed_release"
  ssh "$server" "sudo systemctl restart zeroboot"
  sleep 3
  run_remote_ready_check "$server" || { echo "ROLLBACK FAILED: readiness check failed on $server" >&2; return 1; }
  run_remote_health_check "$server" || { echo "ROLLBACK FAILED: health check failed on $server" >&2; return 1; }
  run_remote_exec_smoke_all "$server" || { echo "ROLLBACK FAILED: exec smoke failed on $server" >&2; return 1; }
}

RELEASE_ARCHIVE="$(mktemp -t zeroboot-release.XXXXXX.tgz)"
ENV_FILE="$(mktemp -t zeroboot-env.XXXXXX)"
trap 'rm -f "$RELEASE_ARCHIVE" "$ENV_FILE"' EXIT
tar -C "$RELEASE_DIR" -czf "$RELEASE_ARCHIVE" .
write_env_file "$ENV_FILE"

for server in $SERVERS; do
  echo "=== Deploying immutable release $RELEASE_ID to $server ==="
  REMOTE_RELEASE_ROOT="$REMOTE_ROOT/releases/$RELEASE_ID"
  TEMPLATE_SPEC="$(build_template_spec "$REMOTE_RELEASE_ROOT")"
  REMOTE_ARCHIVE="/tmp/zeroboot-${RELEASE_ID}.tgz"
  REMOTE_ENV_TMP="/tmp/zeroboot-env-${RELEASE_ID}"
  CURRENT_RELEASE="$(get_remote_current_release "$server")"

  ssh "$server" "sudo mkdir -p '$REMOTE_ROOT/releases' /etc/zeroboot /var/lib/zeroboot && sudo rm -rf '$REMOTE_RELEASE_ROOT' && sudo mkdir -p '$REMOTE_RELEASE_ROOT'"
  scp "$RELEASE_ARCHIVE" "$server:$REMOTE_ARCHIVE"
  scp "$ENV_FILE" "$server:$REMOTE_ENV_TMP"
  scp "$API_KEYS_FILE" "$server:/tmp/api_keys.json"
  scp "$PEPPER_FILE" "$server:/tmp/pepper"
  scp deploy/zeroboot.service "$server:/tmp/zeroboot.service"
  if [[ -f "$KEYRING_FILE" ]]; then
    scp "$KEYRING_FILE" "$server:/tmp/keyring.json"
  fi

  ssh "$server" "sudo tar --no-same-owner -xzf '$REMOTE_ARCHIVE' -C '$REMOTE_RELEASE_ROOT' && sudo rm -f '$REMOTE_ARCHIVE' && sudo chown -R zeroboot:kvm '$REMOTE_RELEASE_ROOT' && sudo find '$REMOTE_RELEASE_ROOT' -type d -exec chmod 0755 {} + && sudo find '$REMOTE_RELEASE_ROOT' -type f -exec chmod 0644 {} + && sudo find '$REMOTE_RELEASE_ROOT/bin' -maxdepth 1 -type f -exec chmod 0755 {} +"
  ssh "$server" "sudo install -m 0600 /tmp/api_keys.json /etc/zeroboot/api_keys.json && sudo rm -f /tmp/api_keys.json"
  ssh "$server" "sudo install -m 0600 /tmp/pepper /etc/zeroboot/pepper && sudo rm -f /tmp/pepper"
  ssh "$server" "sudo install -m 0600 '$REMOTE_ENV_TMP' /etc/zeroboot/env && sudo rm -f '$REMOTE_ENV_TMP'"
  ssh "$server" "sudo install -m 0644 /tmp/zeroboot.service /etc/systemd/system/zeroboot.service && sudo rm -f /tmp/zeroboot.service"
  if [[ -f "$KEYRING_FILE" ]]; then
    ssh "$server" "sudo install -m 0600 /tmp/keyring.json /etc/zeroboot/keyring.json && sudo rm -f /tmp/keyring.json"
  fi

  echo "Verifying staged release receipt and promoted templates..."
  ssh "$server" "python3 - '$REMOTE_RELEASE_ROOT/release-receipt.json' <<'PY'
import json
import pathlib
import sys
receipt_path = pathlib.Path(sys.argv[1])
receipt = json.loads(receipt_path.read_text())
if not receipt.get('release_id'):
    raise SystemExit('release-receipt.json missing release_id')
templates = receipt.get('templates')
if not isinstance(templates, list) or not templates:
    raise SystemExit('release-receipt.json missing templates[]')
for item in templates:
    manifest_path = receipt_path.parent / item['manifest_path']
    if not manifest_path.is_file():
        raise SystemExit(f'missing manifest in staged release: {manifest_path}')
PY"

  run_remote_verify_startup "$server" "$REMOTE_RELEASE_ROOT" "$TEMPLATE_SPEC"

  echo "Cutting over current symlink..."
  ssh "$server" "cd '$REMOTE_ROOT' && sudo ln -sfn 'releases/$RELEASE_ID' current && sudo systemctl daemon-reload"
  update_remote_deploy_state "$server" "$RELEASE_ID" "$CURRENT_RELEASE"
  ssh "$server" "sudo systemctl restart zeroboot"

  sleep 3
  if ! run_remote_ready_check "$server"; then
    echo "Readiness check failed on $server"
    rollback_remote_release "$server" "$RELEASE_ID" "$CURRENT_RELEASE"
    continue
  fi

  if ! run_remote_health_check "$server"; then
    echo "Health check failed on $server"
    rollback_remote_release "$server" "$RELEASE_ID" "$CURRENT_RELEASE"
    continue
  fi

  if ! run_remote_exec_smoke_all "$server"; then
    echo "Exec smoke failed on $server"
    rollback_remote_release "$server" "$RELEASE_ID" "$CURRENT_RELEASE"
    continue
  fi

  echo "Deployment to $server succeeded"
done

echo "=== Deployments complete ==="
