#!/bin/bash
# Remote XBOOT runtime-validation setup for an Ubuntu 22.04 x86_64 KVM host.
# Run this on the local workstation to push the current repo to a remote host
# and execute the Linux/KVM validation flow there.

set -euo pipefail

REMOTE_HOST="${REMOTE_HOST:-192.168.64.6}"
REMOTE_USER="${REMOTE_USER:-root}"
REMOTE_PASS="${REMOTE_PASS:-password}"
REMOTE_ROOT="${REMOTE_ROOT:-/opt/xboot}"
REMOTE_WORK_ROOT="${REMOTE_WORK_ROOT:-/var/lib/zeroboot/validation}"
VALIDATION_PORT="${VALIDATION_PORT:-8080}"
REPEAT_COUNT="${REPEAT_COUNT:-100}"
XBOOT_DIR="${XBOOT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"

TARBALL_NAME="$(basename "$XBOOT_DIR").tar.gz"
LOCAL_TARBALL="${LOCAL_TARBALL:-/tmp/$TARBALL_NAME}"
REMOTE_REPO_DIR="$REMOTE_ROOT/$(basename "$XBOOT_DIR")"

SSH_OPTS=(-o StrictHostKeyChecking=no)
SCP_OPTS=(-o StrictHostKeyChecking=no)

run_ssh() {
    if [[ -n "$REMOTE_PASS" ]]; then
        sshpass -p "$REMOTE_PASS" ssh "${SSH_OPTS[@]}" "$REMOTE_USER@$REMOTE_HOST" "$@"
    else
        ssh "${SSH_OPTS[@]}" "$REMOTE_USER@$REMOTE_HOST" "$@"
    fi
}

run_scp() {
    if [[ -n "$REMOTE_PASS" ]]; then
        sshpass -p "$REMOTE_PASS" scp "${SCP_OPTS[@]}" "$@"
    else
        scp "${SCP_OPTS[@]}" "$@"
    fi
}

echo "=== XBOOT Remote Runtime Validation ==="
echo "Local repo:   $XBOOT_DIR"
echo "Remote host:  $REMOTE_USER@$REMOTE_HOST"
echo "Remote repo:  $REMOTE_REPO_DIR"
echo "Work root:    $REMOTE_WORK_ROOT"
echo ""

if [[ -n "$REMOTE_PASS" ]] && ! command -v sshpass >/dev/null 2>&1; then
    echo "Installing sshpass..."
    brew install sshpass
fi

echo "[1/5] Verifying local baseline..."
(
    cd "$XBOOT_DIR"
    cargo fmt --check
    python3 -m pytest -q tests
    cargo clippy --locked -- -D warnings -A dead_code -A unused_variables -A unused_imports -A clippy::empty_line_after_doc_comments
    cargo test --locked
)

echo "[2/5] Packaging repository..."
rm -f "$LOCAL_TARBALL"
tar -C "$(dirname "$XBOOT_DIR")" \
    --exclude="$(basename "$XBOOT_DIR")/target" \
    --exclude="$(basename "$XBOOT_DIR")/.git" \
    --exclude="$(basename "$XBOOT_DIR")/.pytest_cache" \
    --exclude="$(basename "$XBOOT_DIR")/__pycache__" \
    -czf "$LOCAL_TARBALL" \
    "$(basename "$XBOOT_DIR")"

echo "[3/5] Transferring archive..."
run_scp "$LOCAL_TARBALL" "$REMOTE_USER@$REMOTE_HOST:/tmp/$TARBALL_NAME"

echo "[4/5] Preparing remote host..."
run_ssh bash <<REMOTE_SCRIPT
set -euo pipefail

if [[ "\$(uname -s)" != "Linux" ]]; then
    echo "remote host must be Linux" >&2
    exit 1
fi

if [[ "\$(uname -m)" != "x86_64" ]]; then
    echo "remote host must be x86_64; got \$(uname -m)" >&2
    exit 1
fi

if [[ -f /etc/os-release ]]; then
    . /etc/os-release
    if [[ "\${ID:-}" != "ubuntu" || "\${VERSION_ID:-}" != "22.04" ]]; then
        echo "remote host must be Ubuntu 22.04; got \${PRETTY_NAME:-unknown}" >&2
        exit 1
    fi
fi

SUDO=""
if [[ "\$(id -u)" -ne 0 ]]; then
    if ! command -v sudo >/dev/null 2>&1; then
        echo "sudo is required when not running as root" >&2
        exit 1
    fi
    SUDO="sudo"
fi

\$SUDO apt-get update
\$SUDO apt-get install -y \
    ca-certificates \
    curl \
    git \
    jq \
    python3 \
    python3-pip \
    build-essential \
    gcc \
    e2fsprogs \
    pkg-config \
    libssl-dev \
    tar \
    xz-utils

if [[ ! -x "\$HOME/.cargo/bin/cargo" ]] && ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi

if [[ -f "\$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1090
    source "\$HOME/.cargo/env"
fi

\$SUDO mkdir -p /var/lib/zeroboot /etc/zeroboot "$REMOTE_ROOT"
\$SUDO rm -rf "$REMOTE_REPO_DIR"
\$SUDO tar -xzf "/tmp/$TARBALL_NAME" -C "$REMOTE_ROOT"
\$SUDO chown -R "\$(id -u):\$(id -g)" "$REMOTE_REPO_DIR"
\$SUDO chmod -R u+rwX "$REMOTE_REPO_DIR"
REMOTE_SCRIPT

echo "[5/5] Running Linux/KVM validation..."
run_ssh "cd '$REMOTE_REPO_DIR' && bash scripts/run_kvm_validation.sh --work-root '$REMOTE_WORK_ROOT' --port '$VALIDATION_PORT' --repeat '$REPEAT_COUNT'"

echo ""
echo "=== Remote validation completed ==="
