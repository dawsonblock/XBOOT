#!/bin/bash
# Remote XBOOT Setup Script for Debian VM
# Run this on your macOS host to set up XBOOT in the Debian VM

set -euo pipefail

VM_IP="192.168.64.6"
VM_USER="root"
VM_PASS="password"
XBOOT_DIR="/Users/dawsonblock/Downloads/XBOOT-main-2"

echo "=== XBOOT Remote Setup for Debian VM ==="
echo "VM IP: $VM_IP"
echo ""

# Check if sshpass is installed
if ! command -v sshpass &> /dev/null; then
    echo "Installing sshpass..."
    brew install sshpass
fi

# Create tarball of XBOOT
echo "[1/6] Creating XBOOT tarball..."
cd $(dirname "$XBOOT_DIR")
tar -czf XBOOT-main-2.tar.gz $(basename "$XBOOT_DIR")

# Transfer to VM
echo "[2/6] Transferring XBOOT to VM..."
sshpass -p "$VM_PASS" scp -o StrictHostKeyChecking=no XBOOT-main-2.tar.gz "$VM_USER@$VM_IP:/root/"

# SSH and setup
echo "[3/6] Setting up environment in VM..."
sshpass -p "$VM_PASS" ssh -o StrictHostKeyChecking=no "$VM_USER@$VM_IP" << 'REMOTE_SCRIPT'
#!/bin/bash
set -euo pipefail

echo "Updating system and installing dependencies..."
apt-get update
apt-get install -y curl wget git python3 python3-pip build-essential

# Install Rust
echo "Installing Rust..."
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# Extract XBOOT
echo "Extracting XBOOT..."
cd /root
tar -xzf XBOOT-main-2.tar.gz

# Download Firecracker for ARM64
echo "Downloading Firecracker 1.12.0 for ARM64..."
mkdir -p /root/xboot/bin
cd /root/xboot/bin
curl -LO https://github.com/firecracker-microvm/firecracker/releases/download/v1.12.0/firecracker-v1.12.0-aarch64.tgz
tar -xzf firecracker-v1.12.0-aarch64.tgz
# Find and rename the firecracker binary
find . -name "firecracker*" -type f -executable | head -1 | xargs -I {} mv {} firecracker
chmod +x firecracker

echo "Firecracker installed at: $(pwd)/firecracker"

# Create workspace
echo "Creating workspace..."
mkdir -p /root/xboot/artifacts
mkdir -p /root/xboot/work/python
mkdir -p /root/xboot/work/node

# Note about artifacts
cat << 'EOF'

=== NEXT STEPS ===

1. You need to download VM artifacts:
   - vmlinux (ARM64 Linux kernel)
   - rootfs-python.ext4 (ARM64 rootfs with Python)
   - rootfs-node.ext4 (ARM64 rootfs with Node.js)

   Place them in /root/xboot/artifacts/

2. Build XBOOT:
   cd /root/XBOOT-main-2
   cargo build --release

3. Create templates:
   export ZEROBOOT_FIRECRACKER_BIN=/root/xboot/bin/firecracker
   ./target/release/zeroboot template \
     /root/xboot/artifacts/vmlinux \
     /root/xboot/artifacts/rootfs-python.ext4 \
     /root/xboot/work/python

4. Run smoke tests:
   ZEROBOOT_FIRECRACKER_BIN=/root/xboot/bin/firecracker ./verify.sh

5. Start server:
   ZEROBOOT_FIRECRACKER_BIN=/root/xboot/bin/firecracker \
     ./target/release/zeroboot serve /root/xboot/work/python 8080

EOF

REMOTE_SCRIPT

echo ""
echo "=== Setup Complete ==="
echo ""
echo "SSH into the VM to continue:"
echo "  ssh root@$VM_IP"
echo "  Password: $VM_PASS"
echo ""
echo "Then follow the steps above to complete the setup."
