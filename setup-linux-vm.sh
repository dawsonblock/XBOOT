#!/bin/bash
# XBOOT Linux VM Setup Script
# Run this inside your ArchLinux VM to set up XBOOT

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}=== XBOOT Linux VM Setup ===${NC}"
echo ""

# Check architecture
ARCH=$(uname -m)
echo "Detected architecture: $ARCH"

if [[ "$ARCH" != "aarch64" && "$ARCH" != "x86_64" ]]; then
    echo -e "${RED}Warning: Architecture $ARCH may not be fully supported${NC}"
fi

# 1. Install dependencies
echo -e "${YELLOW}[1/7] Installing dependencies...${NC}"
pacman -Syu --noconfirm
pacman -S --noconfirm rustup git curl wget python python-pip qemu-base

# Initialize Rust
rustup default stable

# 2. Create workspace
echo -e "${YELLOW}[2/7] Creating XBOOT workspace...${NC}"
mkdir -p ~/xboot
cd ~/xboot

# 3. Download XBOOT source from macOS host via shared directory
echo -e "${YELLOW}[3/7] Setting up XBOOT source...${NC}"
echo "Please ensure the XBOOT directory is shared with the VM via UTM's shared directory feature."
echo "Then copy it to ~/xboot/XBOOT-main-2:"
echo "  cp -r /mnt/shared/XBOOT-main-2 ~/xboot/"

# 4. Download Firecracker
echo -e "${YELLOW}[4/7] Downloading Firecracker 1.12.0...${NC}"
mkdir -p ~/xboot/bin
if [[ "$ARCH" == "aarch64" ]]; then
    FIRECRACKER_URL="https://github.com/firecracker-microvm/firecracker/releases/download/v1.12.0/firecracker-v1.12.0-aarch64.tgz"
    FIRECRACKER_BIN="firecracker-v1.12.0-aarch64"
else
    FIRECRACKER_URL="https://github.com/firecracker-microvm/firecracker/releases/download/v1.12.0/firecracker-v1.12.0-x86_64.tgz"
    FIRECRACKER_BIN="firecracker-v1.12.0-x86_64"
fi

if [[ ! -f ~/xboot/bin/firecracker ]]; then
    cd ~/xboot/bin
    curl -LO "$FIRECRACKER_URL"
    tar -xzf "firecracker-v1.12.0-*.tgz" --wildcards '*/firecracker*' --strip-components=1 2>/dev/null || true
    mv firecracker-v* firecracker 2>/dev/null || true
    chmod +x firecracker
    cd ~/xboot
fi

echo -e "${GREEN}Firecracker installed at ~/xboot/bin/firecracker${NC}"

# 5. Download kernel and rootfs
echo -e "${YELLOW}[5/7] Downloading kernel and rootfs images...${NC}"
mkdir -p ~/xboot/artifacts

# For aarch64, we need ARM64 kernel and rootfs
# These are example URLs - you'll need to provide actual artifacts
if [[ "$ARCH" == "aarch64" ]]; then
    echo "Note: For ARM64, you need:"
    echo "  - vmlinux-5.10.223 (ARM64 version)"
    echo "  - rootfs-python.ext4 (ARM64 version with Python)"
    echo "  - rootfs-node.ext4 (ARM64 version with Node.js)"
    echo ""
    echo "Place these in ~/xboot/artifacts/"
else
    echo "Note: For x86_64, you need:"
    echo "  - vmlinux-5.10.223"
    echo "  - rootfs-python.ext4"
    echo "  - rootfs-node.ext4"
    echo ""
    echo "Place these in ~/xboot/artifacts/"
fi

# 6. Build XBOOT (once source is available)
echo -e "${YELLOW}[6/7] Building XBOOT...${NC}"
if [[ -d ~/xboot/XBOOT-main-2 ]]; then
    cd ~/xboot/XBOOT-main-2
    export PATH="$HOME/xboot/bin:$PATH"
    cargo build --release
    echo -e "${GREEN}XBOOT built successfully!${NC}"
else
    echo -e "${YELLOW}XBOOT source not found. Please copy it from the shared directory first.${NC}"
fi

# 7. Create work directories
echo -e "${YELLOW}[7/7] Creating work directories...${NC}"
mkdir -p ~/xboot/work/python
mkdir -p ~/xboot/work/node

cd ~/xboot

echo ""
echo -e "${GREEN}=== Setup Complete ===${NC}"
echo ""
echo "Next steps:"
echo "1. Copy XBOOT source from shared directory:"
echo "   cp -r /mnt/shared/XBOOT-main-2 ~/xboot/"
echo ""
echo "2. Download kernel and rootfs artifacts to ~/xboot/artifacts/"
echo ""
echo "3. Create templates:"
echo "   cd ~/xboot/XBOOT-main-2"
echo "   export PATH=\"\$HOME/xboot/bin:\$PATH\""
echo "   ./target/release/zeroboot template ~/xboot/artifacts/vmlinux ~/xboot/artifacts/rootfs-python.ext4 ~/xboot/work/python"
echo ""
echo "4. Run smoke tests:"
echo "   ZEROBOOT_FIRECRACKER_BIN=\$HOME/xboot/bin/firecracker ./verify.sh"
echo ""
echo "5. Start the server:"
echo "   ZEROBOOT_FIRECRACKER_BIN=\$HOME/xboot/bin/firecracker ./target/release/zeroboot serve ~/xboot/work/python 8080"
