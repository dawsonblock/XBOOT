#!/bin/bash
# XBOOT Release Tree Assembler
# Assembles the release directory structure from build artifacts

set -euo pipefail

# Default paths
SOURCE_DIR="${1:-$(pwd)}"
RELEASE_ROOT="${2:-/var/lib/zeroboot/current}"
KERNEL_PATH="${3:-${SOURCE_DIR}/guest/vmlinux-fc}"

# Template source directories
PYTHON_WORKDIR="${SOURCE_DIR}/work/python"
NODE_WORKDIR="${SOURCE_DIR}/work/node"

# Artifact paths
ARTIFACTS_DIR="/var/lib/zeroboot/artifacts"

echo "=== XBOOT Release Tree Assembler ==="
echo "Source: $SOURCE_DIR"
echo "Release root: $RELEASE_ROOT"
echo ""

# Validate source directory exists
if [[ ! -d "$SOURCE_DIR" ]]; then
    echo "ERROR: Source directory does not exist: $SOURCE_DIR" >&2
    exit 1
fi

# Check for required binaries
echo "=== Validating Build Artifacts ==="

ZEROBOOT_BIN="${SOURCE_DIR}/target/release/zeroboot"
if [[ ! -x "$ZEROBOOT_BIN" ]]; then
    echo "ERROR: zeroboot binary not found at $ZEROBOOT_BIN" >&2
    echo "Run 'make build' first" >&2
    exit 1
fi
echo "OK: zeroboot binary found"

# Check for Python template
if [[ ! -d "$PYTHON_WORKDIR" ]]; then
    echo "ERROR: Python template directory not found at $PYTHON_WORKDIR" >&2
    echo "Run 'make template-python' first" >&2
    exit 1
fi
if [[ ! -f "$PYTHON_WORKDIR/template.manifest.json" ]]; then
    echo "ERROR: Python template manifest not found" >&2
    exit 1
fi
echo "OK: Python template found"

# Check for Node.js template
if [[ ! -d "$NODE_WORKDIR" ]]; then
    echo "ERROR: Node.js template directory not found at $NODE_WORKDIR" >&2
    echo "Run 'make template-node' first" >&2
    exit 1
fi
if [[ ! -f "$NODE_WORKDIR/template.manifest.json" ]]; then
    echo "ERROR: Node.js template manifest not found" >&2
    exit 1
fi
echo "OK: Node.js template found"

# Check for kernel
if [[ ! -f "$KERNEL_PATH" ]]; then
    echo "ERROR: Kernel not found at $KERNEL_PATH" >&2
    exit 1
fi
echo "OK: Kernel found"

echo ""
echo "=== Creating Release Directory Structure ==="

# Create directories
mkdir -p "$RELEASE_ROOT/bin"
mkdir -p "$RELEASE_ROOT/templates/python"
mkdir -p "$RELEASE_ROOT/templates/node"

# Copy zeroboot binary
echo "Copying zeroboot binary..."
cp "$ZEROBOOT_BIN" "$RELEASE_ROOT/bin/"
chmod +x "$RELEASE_ROOT/bin/zeroboot"

# Copy Python template
echo "Copying Python template..."
cp -r "$PYTHON_WORKDIR/"* "$RELEASE_ROOT/templates/python/"

# Copy Node.js template
echo "Copying Node.js template..."
cp -r "$NODE_WORKDIR/"* "$RELEASE_ROOT/templates/node/"

# Verify templates have required files
echo ""
echo "=== Verifying Template Integrity ==="

for lang in python node; do
    TEMPLATE_DIR="$RELEASE_ROOT/templates/$lang"
    
    echo -n "$lang template.manifest.json: "
    if [[ -f "$TEMPLATE_DIR/template.manifest.json" ]]; then
        echo "OK"
    else
        echo "MISSING"
        exit 1
    fi
    
    echo -n "$lang rootfs.ext4: "
    if [[ -f "$TEMPLATE_DIR/rootfs.ext4" ]]; then
        echo "OK"
    else
        echo "MISSING"
        exit 1
    fi
    
    echo -n "$lang kernel: "
    if [[ -f "$TEMPLATE_DIR/vmlinux" ]] || [[ -L "$TEMPLATE_DIR/vmlinux" ]]; then
        echo "OK"
    else
        echo "MISSING"
        exit 1
    fi
    
    echo -n "$lang state dir: "
    if [[ -d "$TEMPLATE_DIR/state" ]]; then
        echo "OK"
    else
        echo "MISSING"
        exit 1
    fi
done

echo ""
echo "=== Generating Release Receipt ==="

RECEIPT_FILE="$RELEASE_ROOT/release-receipt.json"

# Generate receipt using the Python script if available
if [[ -f "${SOURCE_DIR}/scripts/create_release_receipt.py" ]]; then
    python3 "${SOURCE_DIR}/scripts/create_release_receipt.py" \
        --bin "$RELEASE_ROOT/bin/zeroboot" \
        --templates "$RELEASE_ROOT/templates" \
        --output "$RECEIPT_FILE"
else
    # Manual receipt generation
    ZEROBOOT_SHA256=$(sha256sum "$RELEASE_ROOT/bin/zeroboot" | awk '{print $1}')
    PYTHON_SHA256=$(sha256sum "$RELEASE_ROOT/templates/python/template.manifest.json" | awk '{print $1}')
    NODE_SHA256=$(sha256sum "$RELEASE_ROOT/templates/node/template.manifest.json" | awk '{print $1}')
    
    cat > "$RECEIPT_FILE" << EOF
{
  "version": "1.0.0",
  "created_at": "$(date -Iseconds)",
  "binaries": {
    "zeroboot": {
      "path": "bin/zeroboot",
      "sha256": "$ZEROBOOT_SHA256"
    }
  },
  "templates": {
    "python": {
      "path": "templates/python",
      "manifest_sha256": "$PYTHON_SHA256"
    },
    "node": {
      "path": "templates/node",
      "manifest_sha256": "$NODE_SHA256"
    }
  },
  "kernel": {
    "path": "templates/python/vmlinux",
    "source": "$KERNEL_PATH"
  }
}
EOF
fi

echo "Receipt generated: $RECEIPT_FILE"

echo ""
echo "=== Setting Permissions ==="

# Set ownership (will fail if not root, that's OK)
if [[ $EUID -eq 0 ]]; then
    chown -R zeroboot:kvm "$RELEASE_ROOT" 2>/dev/null || true
    chmod 750 "$RELEASE_ROOT"
    chmod -R 750 "$RELEASE_ROOT/bin"
    chmod -R 750 "$RELEASE_ROOT/templates"
    echo "Ownership set to zeroboot:kvm"
else
    echo "WARNING: Not running as root, skipping ownership setup"
    echo "Run as root or manually set ownership: chown -R zeroboot:kvm $RELEASE_ROOT"
fi

echo ""
echo "=== Release Tree Summary ==="
find "$RELEASE_ROOT" -type f -o -type d | sort | while read -r f; do
    if [[ -f "$f" ]]; then
        echo "  [F] $f"
    else
        echo "  [D] $f"
    fi
done

echo ""
echo "=== Release Tree Assembled ==="
echo "Location: $RELEASE_ROOT"
echo ""
echo "Next steps:"
echo "1. Create /etc/zeroboot configuration"
echo "2. Run verify-startup: $RELEASE_ROOT/bin/zeroboot verify-startup \"python:$RELEASE_ROOT/templates/python,node:$RELEASE_ROOT/templates/node\" --release-root $RELEASE_ROOT"
echo "3. Install systemd service: cp deploy/zeroboot.service /etc/systemd/system/"
echo "4. Start service: systemctl start zeroboot"
echo "5. Run smoke test: ./scripts/smoke_exec.sh <api_key>"
