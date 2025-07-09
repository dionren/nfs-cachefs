#!/bin/bash

# NFS-CacheFS v0.2.0 Installation Script
# Supports Ubuntu 22.04/24.04 x86_64

set -e

echo "Installing NFS-CacheFS v0.2.0..."

# Check if running as root
if [[ $EUID -eq 0 ]]; then
    echo "Error: Do not run this script as root. Use sudo when prompted."
    exit 1
fi

# Check system compatibility
if [[ "$(uname -m)" != "x86_64" ]]; then
    echo "Error: This package is only compatible with x86_64 architecture."
    exit 1
fi

# Check Ubuntu version
if ! grep -q "Ubuntu" /etc/os-release; then
    echo "Warning: This package is designed for Ubuntu. Continuing anyway..."
fi

# Install dependencies
echo "Installing dependencies..."
sudo apt update
sudo apt install -y fuse3 libfuse3-3

# Install binary
echo "Installing nfs-cachefs binary..."
sudo cp nfs-cachefs /usr/local/bin/
sudo chmod +x /usr/local/bin/nfs-cachefs

# Create mount helper symlink
echo "Creating mount helper symlink..."
sudo ln -sf /usr/local/bin/nfs-cachefs /sbin/mount.cachefs

# Verify installation
echo "Verifying installation..."
if command -v nfs-cachefs >/dev/null 2>&1; then
    echo "✓ nfs-cachefs installed successfully"
    nfs-cachefs --version
else
    echo "✗ Installation failed"
    exit 1
fi

if [[ -L /sbin/mount.cachefs ]]; then
    echo "✓ mount.cachefs helper installed successfully"
else
    echo "✗ Mount helper installation failed"
    exit 1
fi

echo ""
echo "Installation completed successfully!"
echo ""
echo "Usage examples:"
echo "1. Manual mount:"
echo "   sudo mount -t cachefs cachefs /mnt/cached \\"
echo "     -o nfs_backend=/mnt/nfs,cache_dir=/mnt/cache,cache_size_gb=50,allow_other"
echo ""
echo "2. Add to /etc/fstab for automatic mounting:"
echo "   cachefs /mnt/cached cachefs nfs_backend=/mnt/nfs,cache_dir=/mnt/cache,cache_size_gb=50,allow_other,_netdev 0 0"
echo ""
echo "For more information, see: https://github.com/your-org/nfs-cachefs"