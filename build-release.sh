#!/bin/bash
# Build release package for NFS-CacheFS

set -e

# Get version from Cargo.toml
VERSION=$(grep '^version = ' Cargo.toml | sed 's/version = "\(.*\)"/\1/')
echo "Building release for version ${VERSION}..."

# Clean previous builds
echo "Cleaning previous builds..."
cargo clean

# Build optimized release
echo "Building optimized release binary..."
cargo build --release

# Create release directory
RELEASE_DIR="nfs-cachefs-v${VERSION}-linux-x86_64"
rm -rf "${RELEASE_DIR}"
mkdir -p "${RELEASE_DIR}"

# Copy binary and other files
echo "Copying files to release directory..."
cp target/release/nfs-cachefs "${RELEASE_DIR}/"
cp README.md "${RELEASE_DIR}/"
cp LICENSE "${RELEASE_DIR}/"
cp CHANGELOG.md "${RELEASE_DIR}/"
cp install.sh "${RELEASE_DIR}/"
cp mount.cachefs "${RELEASE_DIR}/"

# Create docs directory in release
mkdir -p "${RELEASE_DIR}/docs"
cp -r docs/* "${RELEASE_DIR}/docs/" 2>/dev/null || true

# Create tarball
echo "Creating tarball..."
tar -czf "${RELEASE_DIR}.tar.gz" "${RELEASE_DIR}"

# Generate checksum
echo "Generating checksum..."
sha256sum "${RELEASE_DIR}.tar.gz" > "${RELEASE_DIR}.tar.gz.sha256"

# Clean up temporary directory
rm -rf "${RELEASE_DIR}"

echo "Release package created successfully:"
echo "  - ${RELEASE_DIR}.tar.gz"
echo "  - ${RELEASE_DIR}.tar.gz.sha256"