#!/bin/bash
# Quick release script for NFS-CacheFS

set -e

# Check if version is provided
if [ $# -eq 0 ]; then
    echo "Usage: ./release.sh <version>"
    echo "Example: ./release.sh 0.3.0"
    exit 1
fi

VERSION=$1
echo "Preparing release v${VERSION}..."

# Run tests
echo "Running tests..."
cargo test

# Build release
echo "Building release package..."
./build-release.sh

# Verify files exist
if [ ! -f "nfs-cachefs-v${VERSION}-linux-x86_64.tar.gz" ]; then
    echo "Error: Release package not found!"
    exit 1
fi

if [ ! -f "nfs-cachefs-v${VERSION}-linux-x86_64.tar.gz.sha256" ]; then
    echo "Error: Checksum file not found!"
    exit 1
fi

echo ""
echo "Release files created successfully:"
echo "  - nfs-cachefs-v${VERSION}-linux-x86_64.tar.gz"
echo "  - nfs-cachefs-v${VERSION}-linux-x86_64.tar.gz.sha256"
echo ""
echo "Next steps:"
echo "1. Commit all changes: git add . && git commit -m 'Release v${VERSION}'"
echo "2. Create tag: git tag -a v${VERSION} -m 'Release version ${VERSION}'"
echo "3. Push to GitHub: git push origin main && git push origin v${VERSION}"
echo "4. Upload the release files to GitHub Releases"
echo ""
echo "GitHub Release URL: https://github.com/yourusername/nfs-cachefs/releases/new"