#!/bin/bash

echo "Testing io_uring implementation for NFS-CacheFS"
echo "=============================================="

# Check if io_uring feature is enabled
echo "1. Checking if io_uring feature is enabled in Cargo.toml..."
grep -n "io_uring" Cargo.toml || echo "io_uring feature not found in Cargo.toml"

# Check kernel support
echo -e "\n2. Checking kernel support for io_uring..."
uname -r
if command -v grep &> /dev/null; then
    if grep -q "CONFIG_IO_URING=y" /boot/config-$(uname -r) 2>/dev/null; then
        echo "✓ Kernel has io_uring support enabled"
    else
        echo "✗ Kernel may not have io_uring support"
    fi
fi

# Build with io_uring feature
echo -e "\n3. Building with io_uring feature..."
cargo build --release --features io_uring

# Create test environment
echo -e "\n4. Setting up test environment..."
TEST_DIR="/tmp/nfs-cachefs-test"
NFS_DIR="$TEST_DIR/nfs"
CACHE_DIR="$TEST_DIR/cache"
MOUNT_DIR="$TEST_DIR/mount"

mkdir -p "$NFS_DIR" "$CACHE_DIR" "$MOUNT_DIR"

# Create test files
echo "Creating test files..."
dd if=/dev/urandom of="$NFS_DIR/small_file.bin" bs=1M count=5 2>/dev/null
dd if=/dev/urandom of="$NFS_DIR/medium_file.bin" bs=1M count=50 2>/dev/null
dd if=/dev/urandom of="$NFS_DIR/large_file.bin" bs=1M count=200 2>/dev/null

echo -e "\nTest files created:"
ls -lh "$NFS_DIR"

# Mount with io_uring enabled
echo -e "\n5. Mounting CacheFS with io_uring enabled..."
MOUNT_CMD="./target/release/nfs-cachefs $MOUNT_DIR \
    -o nfs_backend=$NFS_DIR \
    -o cache_dir=$CACHE_DIR \
    -o cache_size_gb=1 \
    -o use_io_uring=true \
    -o log_level=debug \
    -o allow_other"

echo "Mount command: $MOUNT_CMD"

# Function to test file copy and check logs
test_file_copy() {
    local filename=$1
    echo -e "\nTesting $filename..."
    
    # Clear cache
    rm -rf "$CACHE_DIR"/*
    
    # Read file to trigger caching
    echo "Reading file to trigger cache..."
    cat "$MOUNT_DIR/$filename" > /dev/null &
    READ_PID=$!
    
    # Monitor logs for io_uring usage
    sleep 2
    
    # Check if io_uring was used
    if pgrep -f "nfs-cachefs" > /dev/null; then
        echo "CacheFS is running, checking for io_uring usage..."
        # In real usage, you would check the debug logs
    fi
    
    wait $READ_PID
    
    # Check if file was cached
    if [ -f "$CACHE_DIR/$filename" ]; then
        echo "✓ File cached successfully"
        CACHED_SIZE=$(stat -c%s "$CACHE_DIR/$filename")
        ORIG_SIZE=$(stat -c%s "$NFS_DIR/$filename")
        if [ "$CACHED_SIZE" -eq "$ORIG_SIZE" ]; then
            echo "✓ File size matches: $CACHED_SIZE bytes"
        else
            echo "✗ File size mismatch! Original: $ORIG_SIZE, Cached: $CACHED_SIZE"
        fi
    else
        echo "✗ File not found in cache"
    fi
}

echo -e "\n6. Instructions for testing:"
echo "   a) Run this script to set up the test environment"
echo "   b) In another terminal, run the mount command shown above"
echo "   c) In a third terminal, monitor the logs with:"
echo "      journalctl -f | grep -E 'CACHE IO_URING|io_uring'"
echo "   d) Then run: $0 test"
echo ""
echo "The logs should show messages like:"
echo "   '🚀 CACHE IO_URING: ... using zero-copy splice'"
echo "   '✨ CACHE IO_URING COMPLETE: ... spliced in ...'"

if [ "$1" == "test" ]; then
    echo -e "\n7. Running tests..."
    test_file_copy "small_file.bin"
    test_file_copy "medium_file.bin" 
    test_file_copy "large_file.bin"
    
    echo -e "\n8. Cleanup..."
    fusermount -u "$MOUNT_DIR" 2>/dev/null
    echo "Done!"
fi