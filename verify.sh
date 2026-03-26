#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BINARY="$SCRIPT_DIR/target/release/zeroboot"
PY_WORKDIR="${PY_WORKDIR:-$SCRIPT_DIR/work/python}"
NODE_WORKDIR="${NODE_WORKDIR:-$SCRIPT_DIR/work/node}"
PASS=0
FAIL=0
RESULTS=()

pass() { RESULTS+=("[PASS] $1"); PASS=$((PASS+1)); }
fail() { RESULTS+=("[FAIL] $1"); FAIL=$((FAIL+1)); }

need_binary() {
  command -v cargo >/dev/null 2>&1 || {
    echo 'cargo is required to run verify.sh' >&2
    exit 1
  }
}

run_exec() {
  local workdir="$1"
  local language="$2"
  shift 2
  timeout 15 "$BINARY" test-exec "$workdir" "$language" "$*" 2>/dev/null || true
}

extract_stdout() {
  awk '
    /^=== stdout ===$/ {in_stdout=1; next}
    /^=== stderr ===$/ {in_stdout=0; next}
    /^exit_code=/ {in_stdout=0}
    in_stdout {print}
  '
}

extract_exit_code() {
  awk -F'[ =]' '/^exit_code=/{print $2}' | tail -1
}

echo '[1/4] building'
cd "$SCRIPT_DIR"
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
need_binary
cargo build --release >/dev/null

echo '[2/4] smoke tests'
PY_OUT="$(run_exec "$PY_WORKDIR" python 'print(1+1)')"
if [[ "$(printf '%s
' "$PY_OUT" | extract_stdout | tr -d '' | xargs)" == '2' && "$(printf '%s
' "$PY_OUT" | extract_exit_code)" == '0' ]]; then
  pass 'python framed exec'
else
  fail 'python framed exec'
fi

TIMEOUT_OUT="$(run_exec "$PY_WORKDIR" python 'while True: pass')"
if [[ "$(printf '%s
' "$TIMEOUT_OUT" | extract_exit_code)" == '-1' ]]; then
  pass 'timeout path'
else
  fail 'timeout path'
fi

NODE_OUT="$(run_exec "$NODE_WORKDIR" node 'console.log(1+1)')"
if [[ "$(printf '%s
' "$NODE_OUT" | extract_stdout | tr -d '' | xargs)" == '2' && "$(printf '%s
' "$NODE_OUT" | extract_exit_code)" == '0' ]]; then
  pass 'node framed exec'
else
  fail 'node framed exec'
fi

echo '[3/4] reporting'
for line in "${RESULTS[@]}"; do echo "  $line"; done

echo '[4/4] summary'
echo "passed=$PASS failed=$FAIL"
[[ $FAIL -eq 0 ]]
