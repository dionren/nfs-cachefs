# NFS-CacheFS v0.2.0 Release Notes

## 🚀 Critical Bug Fix Release

This release fixes a critical issue with mount helper mode parameter parsing that prevented proper mounting using standard mount commands.

## 🔧 Fixed Issues

### Mount Helper Parameter Parsing
- **Fixed**: Mount commands now correctly parse `-o` options when using `mount -t cachefs`
- **Fixed**: `nfs_backend` parameter not being read from mount options  
- **Fixed**: Argument parsing logic for mount.cachefs helper mode

## ✨ What's New

### Enhanced Mount Support
- Improved mount helper detection and parameter processing
- Better error messages for mount parameter validation
- Full support for standard mount command usage

### Technical Improvements
- Fixed loop logic in parameter parsing functions
- Corrected parameter extraction from `-o` option string
- Added proper support for comma-separated mount options
- Enhanced key-value pair parsing for mount parameters

## 📦 Installation

### Download
- **Binary Package**: `nfs-cachefs-v0.2.0-linux-x86_64.tar.gz`
- **SHA256**: `bb4dd5ac683982e867f40c7d312d832729b69c272a3c696d115eed5b4a4c6aa3`
- **Size**: ~1.0 MB

### System Requirements
- **OS**: Ubuntu 22.04/24.04 LTS
- **Architecture**: x86_64 (64-bit)
- **Dependencies**: libfuse3-3, fuse3

### Quick Install
```bash
# Download and extract
wget https://github.com/your-org/nfs-cachefs/releases/download/v0.2.0/nfs-cachefs-v0.2.0-linux-x86_64.tar.gz
tar -xzf nfs-cachefs-v0.2.0-linux-x86_64.tar.gz
cd nfs-cachefs-v0.2.0-linux-x86_64

# Install
./install.sh
```

## 🎯 Usage

Mount commands now work correctly:

```bash
# Manual mount
sudo mount -t cachefs cachefs /mnt/cached \
    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/cache,cache_size_gb=50,allow_other

# Or add to /etc/fstab for automatic mounting
cachefs /mnt/cached cachefs nfs_backend=/mnt/nfs,cache_dir=/mnt/cache,cache_size_gb=50,allow_other,_netdev 0 0
```

## 📋 Supported Mount Options

### Required Parameters
- `nfs_backend=/path/to/nfs` - NFS backend directory path
- `cache_dir=/path/to/cache` - Local cache directory path

### Optional Parameters
- `cache_size_gb=50` - Cache size in GB (default: 10)
- `block_size_mb=64` - Cache block size in MB (default: 64)
- `max_concurrent=10` - Maximum concurrent caching tasks (default: 10)
- `allow_other` - Allow other users to access the mount point
- `allow_root` - Allow root user to access the mount point
- `auto_unmount` - Automatically unmount when process exits

## 🔄 Migration from v0.1.x

This release is fully backward compatible. Simply replace the binary and mount helper:

```bash
# Stop any running instances
sudo umount /mnt/cached

# Replace binary
sudo cp nfs-cachefs /usr/local/bin/
sudo ln -sf /usr/local/bin/nfs-cachefs /sbin/mount.cachefs

# Remount
sudo mount -t cachefs cachefs /mnt/cached -o ...
```

## 🐛 Known Issues

- FUSE mount requires `fuse3` package to be installed
- Only read-only mode is supported (write operations will fail)
- Requires NFS backend to be mounted before CacheFS

## 🔗 Links

- **Repository**: https://github.com/your-org/nfs-cachefs
- **Documentation**: See README.md in the package
- **Issues**: https://github.com/your-org/nfs-cachefs/issues

---

**Full Changelog**: https://github.com/your-org/nfs-cachefs/compare/v0.1.0...v0.2.0