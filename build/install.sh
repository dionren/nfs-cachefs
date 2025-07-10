#!/bin/bash

# NFS-CacheFS Installation Script
# Supports Linux x86_64 systems with static binary

set -e

echo "Installing NFS-CacheFS..."

# Check if running as root and set sudo prefix accordingly
if [[ $EUID -eq 0 ]]; then
    echo "Running as root user"
    SUDO_PREFIX=""
else
    echo "Running as non-root user, will use sudo when needed"
    SUDO_PREFIX="sudo"
fi

# Check system compatibility
if [[ "$(uname -m)" != "x86_64" ]]; then
    echo "Error: This package is only compatible with x86_64 architecture."
    exit 1
fi

# Check if nfs-cachefs binary exists
if [[ ! -f "nfs-cachefs" ]]; then
    echo "Error: nfs-cachefs binary not found in current directory."
    echo "Please make sure you are in the extracted release directory."
    exit 1
fi

# Install dependencies (FUSE is still needed for runtime)
echo "Installing dependencies..."
if command -v apt &> /dev/null; then
    $SUDO_PREFIX apt update
    $SUDO_PREFIX apt install -y fuse3 || $SUDO_PREFIX apt install -y fuse
elif command -v yum &> /dev/null; then
    $SUDO_PREFIX yum install -y fuse3 || $SUDO_PREFIX yum install -y fuse
elif command -v dnf &> /dev/null; then
    $SUDO_PREFIX dnf install -y fuse3 || $SUDO_PREFIX dnf install -y fuse
else
    echo "Warning: Could not detect package manager. Please ensure FUSE is installed."
fi

# Install binary
echo "Installing nfs-cachefs binary..."
$SUDO_PREFIX cp nfs-cachefs /usr/local/bin/
$SUDO_PREFIX chmod +x /usr/local/bin/nfs-cachefs

# Create mount helper symlink
echo "Creating mount helper symlink..."
$SUDO_PREFIX ln -sf /usr/local/bin/nfs-cachefs /sbin/mount.cachefs

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
if [[ $EUID -eq 0 ]]; then
    echo "   mount -t cachefs cachefs /mnt/cached \\"
else
    echo "   sudo mount -t cachefs cachefs /mnt/cached \\"
fi
echo "     -o nfs_backend=/mnt/nfs,cache_dir=/mnt/cache,cache_size_gb=50,allow_other"
echo ""
echo "2. Add to /etc/fstab for automatic mounting:"
echo "   cachefs /mnt/cached cachefs nfs_backend=/mnt/nfs,cache_dir=/mnt/cache,cache_size_gb=50,allow_other,_netdev 0 0"
echo ""
echo "Note: This version uses static linking and works on any Linux system."
echo "For more information, see: https://github.com/your-org/nfs-cachefs" 