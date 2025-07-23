#!/bin/bash
# Test script for io_uring build

set -e

echo "========================================="
echo "Testing NFS-CacheFS io_uring Integration"
echo "========================================="

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Test 1: Build without io_uring feature
echo -e "\n${YELLOW}Test 1: Building without io_uring feature...${NC}"
cargo clean
cargo build --release
if [ $? -eq 0 ]; then
    echo -e "${GREEN}✅ Build without io_uring succeeded${NC}"
else
    echo -e "${RED}❌ Build without io_uring failed${NC}"
    exit 1
fi

# Test 2: Build with io_uring feature
echo -e "\n${YELLOW}Test 2: Building with io_uring feature...${NC}"
cargo clean
cargo build --release --features io_uring
if [ $? -eq 0 ]; then
    echo -e "${GREEN}✅ Build with io_uring succeeded${NC}"
else
    echo -e "${RED}❌ Build with io_uring failed${NC}"
    exit 1
fi

# Test 3: Run tests without io_uring
echo -e "\n${YELLOW}Test 3: Running tests without io_uring...${NC}"
cargo test
if [ $? -eq 0 ]; then
    echo -e "${GREEN}✅ Tests without io_uring passed${NC}"
else
    echo -e "${RED}❌ Tests without io_uring failed${NC}"
fi

# Test 4: Run tests with io_uring
echo -e "\n${YELLOW}Test 4: Running tests with io_uring...${NC}"
cargo test --features io_uring
if [ $? -eq 0 ]; then
    echo -e "${GREEN}✅ Tests with io_uring passed${NC}"
else
    echo -e "${RED}❌ Tests with io_uring failed${NC}"
fi

# Test 5: Check binary size difference
echo -e "\n${YELLOW}Test 5: Comparing binary sizes...${NC}"
cargo build --release
SIZE_WITHOUT=$(stat -c%s target/release/nfs-cachefs)
cargo build --release --features io_uring
SIZE_WITH=$(stat -c%s target/release/nfs-cachefs)

echo "Binary size without io_uring: $(numfmt --to=iec-i --suffix=B $SIZE_WITHOUT)"
echo "Binary size with io_uring: $(numfmt --to=iec-i --suffix=B $SIZE_WITH)"
DIFF=$((SIZE_WITH - SIZE_WITHOUT))
echo "Difference: $(numfmt --to=iec-i --suffix=B $DIFF)"

# Test 6: Check io_uring support
echo -e "\n${YELLOW}Test 6: Checking io_uring kernel support...${NC}"
if [ -f /proc/kallsyms ]; then
    if grep -q "io_uring" /proc/kallsyms; then
        echo -e "${GREEN}✅ Kernel has io_uring support${NC}"
        # Get kernel version
        KERNEL_VERSION=$(uname -r)
        echo "Kernel version: $KERNEL_VERSION"
    else
        echo -e "${YELLOW}⚠️  io_uring not found in kernel symbols${NC}"
    fi
else
    echo -e "${YELLOW}⚠️  Cannot check kernel support (no access to /proc/kallsyms)${NC}"
fi

echo -e "\n${GREEN}========================================="
echo "All build tests completed successfully!"
echo "=========================================${NC}"

# Optional: Show feature flags
echo -e "\n${YELLOW}Binary capabilities:${NC}"
echo "Without io_uring:"
./target/release/nfs-cachefs --version 2>/dev/null || echo "  (no version info)"

echo -e "\nWith io_uring:"
cargo build --release --features io_uring
./target/release/nfs-cachefs --version 2>/dev/null || echo "  (no version info)"