#!/bin/bash
# Build script for NFS-CacheFS release

set -e

VERSION="0.3.0"
RELEASE_NAME="nfs-cachefs-v${VERSION}-linux-x86_64"
BUILD_DIR="build"
RELEASE_DIR="${BUILD_DIR}/${RELEASE_NAME}"

echo "Building NFS-CacheFS v${VERSION}..."

# Clean previous builds
rm -rf ${BUILD_DIR}
mkdir -p ${RELEASE_DIR}

# Build release binary
echo "Building release binary..."
cargo build --release

# Check if build was successful
if [ ! -f "target/release/nfs-cachefs" ]; then
    echo "Error: Build failed!"
    exit 1
fi

# Copy binary
echo "Copying binary..."
cp target/release/nfs-cachefs ${RELEASE_DIR}/

# Copy documentation and scripts
echo "Copying documentation..."
cp README.md ${RELEASE_DIR}/
cp LICENSE ${RELEASE_DIR}/
cp CHANGELOG.md ${RELEASE_DIR}/
cp install.sh ${RELEASE_DIR}/

# Make install script executable
chmod +x ${RELEASE_DIR}/install.sh

# Create tarball
echo "Creating release tarball..."
cd ${BUILD_DIR}
tar -czf ${RELEASE_NAME}.tar.gz ${RELEASE_NAME}

# Create checksum
echo "Creating checksum..."
sha256sum ${RELEASE_NAME}.tar.gz > ${RELEASE_NAME}.tar.gz.sha256

# Move to project root
mv ${RELEASE_NAME}.tar.gz ../
mv ${RELEASE_NAME}.tar.gz.sha256 ../

cd ..

echo "Release build complete!"
echo "Files created:"
echo "  - ${RELEASE_NAME}.tar.gz"
echo "  - ${RELEASE_NAME}.tar.gz.sha256"
echo ""
echo "To verify checksum:"
echo "  sha256sum -c ${RELEASE_NAME}.tar.gz.sha256"