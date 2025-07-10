#!/bin/bash
# Test script for NFS-CacheFS v0.3.0 mount fix

set -e

echo "=== NFS-CacheFS Mount Test ==="
echo "Testing if mount command returns immediately..."

# Create test directories
TEST_DIR="/tmp/nfs-cachefs-test-$$"
mkdir -p "$TEST_DIR/nfs"
mkdir -p "$TEST_DIR/cache"
mkdir -p "$TEST_DIR/mount"

echo "Created test directories:"
echo "  NFS backend: $TEST_DIR/nfs"
echo "  Cache dir: $TEST_DIR/cache"
echo "  Mount point: $TEST_DIR/mount"

# Install the binary
echo -e "\nInstalling NFS-CacheFS..."
tar -xzf nfs-cachefs-v0.3.0-linux-x86_64.tar.gz
sudo cp nfs-cachefs /usr/local/bin/
sudo ln -sf /usr/local/bin/nfs-cachefs /sbin/mount.cachefs

# Test mount with timeout
echo -e "\nTesting mount command (should return within 5 seconds)..."
MOUNT_CMD="sudo mount -t cachefs cachefs $TEST_DIR/mount -o nfs_backend=$TEST_DIR/nfs,cache_dir=$TEST_DIR/cache,cache_size_gb=1,allow_other"

# Run mount with timeout
if timeout 5s $MOUNT_CMD; then
    echo "✓ Mount command returned successfully!"
    
    # Check if filesystem is mounted
    if mount | grep -q "$TEST_DIR/mount"; then
        echo "✓ Filesystem is mounted"
        
        # Test basic operations
        echo -e "\nTesting basic operations..."
        echo "test" | sudo tee "$TEST_DIR/mount/test.txt" > /dev/null
        if [ -f "$TEST_DIR/mount/test.txt" ]; then
            echo "✓ File creation works"
        fi
        
        # Unmount
        echo -e "\nUnmounting..."
        sudo umount "$TEST_DIR/mount"
        echo "✓ Unmount successful"
    else
        echo "✗ Filesystem not found in mount list"
    fi
else
    echo "✗ Mount command timed out (hanging issue not fixed)"
fi

# Test foreground mode
echo -e "\nTesting foreground mode..."
MOUNT_FG_CMD="sudo mount -t cachefs cachefs $TEST_DIR/mount -o nfs_backend=$TEST_DIR/nfs,cache_dir=$TEST_DIR/cache,cache_size_gb=1,allow_other,foreground"

# Run in background and kill after 2 seconds
$MOUNT_FG_CMD &
MOUNT_PID=$!
sleep 2
if kill -0 $MOUNT_PID 2>/dev/null; then
    echo "✓ Foreground mode keeps process running (as expected)"
    sudo kill $MOUNT_PID
    sleep 1
fi

# Cleanup
echo -e "\nCleaning up..."
sudo rm -f /sbin/mount.cachefs
sudo rm -f /usr/local/bin/nfs-cachefs
rm -rf "$TEST_DIR"
rm -rf nfs-cachefs docs/

echo -e "\n=== Test Complete ==="