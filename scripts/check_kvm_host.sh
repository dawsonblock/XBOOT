#!/bin/bash
# XBOOT Host Readiness Check
# Verifies that the host is ready to run XBOOT workloads

set -euo pipefail

ERRORS=0
WARNINGS=0

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "ERROR: Missing required command: $1" >&2
        ERRORS=$((ERRORS + 1))
        return 1
    }
}

echo "=== XBOOT Host Readiness Check ==="
echo "Date: $(date -Iseconds)"
echo "Host: $(hostname)"
echo ""

# Check OS and architecture
echo "=== System ==="
echo -n "OS: "
if [[ -f /etc/os-release ]]; then
    source /etc/os-release
    echo "$NAME $VERSION_ID"
    if [[ "$NAME" != *"Ubuntu"* ]] || [[ "$VERSION_ID" != "22.04" ]]; then
        echo "WARNING: Expected Ubuntu 22.04, got $NAME $VERSION_ID" >&2
        WARNINGS=$((WARNINGS + 1))
    fi
else
    echo "Unknown"
    WARNINGS=$((WARNINGS + 1))
fi

echo -n "Architecture: "
ARCH=$(uname -m)
echo "$ARCH"
if [[ "$ARCH" != "x86_64" ]]; then
    echo "ERROR: x86_64 required, got $ARCH" >&2
    ERRORS=$((ERRORS + 1))
fi

echo ""
echo "=== Dependencies ==="

# Check required commands
need curl
need sha256sum
need python3
need jq
need tar
need mkfs.ext4

# Check Rust toolchain
echo -n "Rust toolchain: "
if command -v rustc >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
    RUST_VERSION=$(rustc --version 2>/dev/null | awk '{print $2}')
    echo "OK ($RUST_VERSION)"
else
    echo "MISSING"
    ERRORS=$((ERRORS + 1))
fi

echo ""
echo "=== KVM Support ==="

# Check /dev/kvm
echo -n "/dev/kvm exists: "
if [[ -e /dev/kvm ]]; then
    echo "YES"
else
    echo "NO"
    ERRORS=$((ERRORS + 1))
fi

echo -n "/dev/kvm readable: "
if [[ -r /dev/kvm ]]; then
    echo "YES"
else
    echo "NO"
    ERRORS=$((ERRORS + 1))
fi

echo -n "/dev/kvm writable: "
if [[ -w /dev/kvm ]]; then
    echo "YES"
else
    echo "NO"
    ERRORS=$((ERRORS + 1))
fi

echo -n "KVM kernel module: "
if lsmod | grep -q kvm; then
    echo "OK"
else
    echo "NOT LOADED"
    ERRORS=$((ERRORS + 1))
fi

echo ""
echo "=== cgroup v2 ==="

# Check cgroup v2
echo -n "cgroup v2 enabled: "
if [[ -f /sys/fs/cgroup/cgroup.controllers ]]; then
    echo "YES"
    echo -n "Available controllers: "
    cat /sys/fs/cgroup/cgroup.controllers
else
    echo "NO (cgroup v1 or hybrid)"
    ERRORS=$((ERRORS + 1))
fi

echo ""
echo "=== Firecracker ==="

# Check Firecracker
echo -n "Firecracker installed: "
if command -v firecracker >/dev/null 2>&1; then
    echo "YES"
    echo -n "Firecracker version: "
    firecracker --version 2>/dev/null || echo "Unknown"
else
    echo "NO"
    WARNINGS=$((WARNINGS + 1))
fi

echo ""
echo "=== User/Group ==="

# Check zeroboot user
echo -n "zeroboot user exists: "
if id -u zeroboot >/dev/null 2>&1; then
    echo "YES"
else
    echo "NO (will be created during setup)"
fi

# Check kvm group membership
echo -n "Current user in kvm group: "
if id -nG "$USER" | grep -qw kvm; then
    echo "YES"
else
    echo "NO"
    WARNINGS=$((WARNINGS + 1))
fi

echo ""
echo "=== Directories ==="

# Check required directories
check_dir() {
    local dir="$1"
    local create="${2:-}"
    echo -n "$dir: "
    if [[ -d "$dir" ]]; then
        if [[ -w "$dir" ]]; then
            echo "OK (exists, writable)"
        else
            echo "NOT WRITABLE"
            ERRORS=$((ERRORS + 1))
        fi
    else
        if [[ "$create" == "create" ]]; then
            echo "MISSING (will create)"
        else
            echo "MISSING"
            ERRORS=$((ERRORS + 1))
        fi
    fi
}

check_dir "/var/lib/zeroboot" "create"
check_dir "/etc/zeroboot" "create"
check_dir "/opt/xboot"

echo ""
echo "=== Summary ==="

if [[ $ERRORS -eq 0 && $WARNINGS -eq 0 ]]; then
    echo "STATUS: READY - Host is ready for XBOOT deployment"
    exit 0
elif [[ $ERRORS -eq 0 ]]; then
    echo "STATUS: READY_WITH_WARNINGS - Host is ready but has $WARNINGS warning(s)"
    exit 0
else
    echo "STATUS: NOT_READY - Host has $ERRORS error(s) and $WARNINGS warning(s)"
    echo ""
    echo "Fix the errors above before proceeding with XBOOT deployment"
    exit 1
fi
