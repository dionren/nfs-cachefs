# NFS-CacheFS v0.4.0 Release Notes

## 🚀 What's New

### Added
- **Automated Release Process**: Complete automated build and packaging system with binary compilation
- **GitHub Actions Integration**: Ready for automated releases through CI/CD
- **Comprehensive Build Scripts**: New `build-release.sh` script for optimized production builds
- **Enhanced Release Packaging**: All necessary files included in release packages

### Improved
- **Build Optimization**: Enhanced release builds with better optimization for production environments
- **Deployment Process**: Streamlined deployment with automated packaging and checksums
- **Documentation**: Updated changelog and release documentation

### Technical Details
- **Binary Size**: Optimized release binary (~2.3MB compressed)
- **Dependencies**: All dependencies properly packaged
- **Checksums**: SHA256 checksums included for integrity verification
- **Platform**: Linux x86_64 support

## 📦 Installation

### Quick Install
```bash
# Download and extract
wget https://github.com/yourusername/nfs-cachefs/releases/download/v0.4.0/nfs-cachefs-v0.4.0-linux-x86_64.tar.gz
tar -xzf nfs-cachefs-v0.4.0-linux-x86_64.tar.gz
cd nfs-cachefs-v0.4.0-linux-x86_64

# Install
sudo ./install.sh
```

### Verify Checksum
```bash
# Verify download integrity
sha256sum -c nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256
```

## 🔧 Usage

```bash
# Basic usage
nfs-cachefs /path/to/nfs/share /path/to/mountpoint

# With custom cache settings
nfs-cachefs /nfs/share /mnt/cache --cache-size 20 --cache-dir /tmp/nfs-cache
```

## 📋 What's Included

- `nfs-cachefs` - Main binary executable
- `install.sh` - Installation script
- `mount.cachefs` - Mount helper
- `README.md` - Documentation
- `LICENSE` - License information
- `CHANGELOG.md` - Version history
- `docs/` - Additional documentation

## 🧪 Testing

All tests pass successfully:
- ✅ 24 library tests
- ✅ 2 integration tests
- ✅ Doc tests

## 🔍 System Requirements

- Linux x86_64
- FUSE3 or FUSE2 development libraries
- Sufficient disk space for cache

## 🐛 Known Issues

None reported for this release.

## 🤝 Contributing

We welcome contributions! Please see our documentation for development setup and contribution guidelines.

## 📄 License

This project is licensed under the terms included in the LICENSE file.

---

**Full Changelog**: https://github.com/yourusername/nfs-cachefs/compare/v0.3.0...v0.4.0