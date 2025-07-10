# NFS-CacheFS v0.3.0 Release Notes

## Overview

NFS-CacheFS v0.3.0 brings significant improvements to the mounting experience, fixing the common "hanging mount" issue and adding automatic background running capabilities.

## Key Features

### 🚀 Automatic Background Running
- Mount commands now automatically fork to background
- No more terminal hanging when mounting
- Seamless integration with system mount utilities

### 🔧 Enhanced Mount Helper
- Proper daemonization support
- Compatible with standard mount command syntax
- Optional `foreground` flag for debugging

### 📚 Improved Documentation
- Comprehensive troubleshooting guide
- Multiple mounting solutions documented
- Clear examples for different use cases

## What's New

### Fixed: Mount Command Hanging
The most significant fix in this release addresses the mount hanging issue. When using:
```bash
mount -t cachefs cachefs /mnt/cached -o nfs_backend=/mnt/nfs,cache_dir=/mnt/cache,cache_size_gb=100,allow_other
```
The command now returns immediately while NFS-CacheFS continues running in the background.

### Added: Foreground Option
For debugging purposes, you can still run in foreground mode:
```bash
mount -t cachefs cachefs /mnt/cached -o nfs_backend=/mnt/nfs,cache_dir=/mnt/cache,foreground
```

### Improved: Error Handling
- Better error messages during mount failures
- Enhanced logging with thread IDs
- Graceful shutdown on signals

## Installation

### Ubuntu 22.04/24.04 (x86_64)

```bash
# Download the release
wget https://github.com/yourusername/nfs-cachefs/releases/download/v0.3.0/nfs-cachefs-v0.3.0-linux-x86_64.tar.gz

# Extract and install
tar -xzf nfs-cachefs-v0.3.0-linux-x86_64.tar.gz
cd nfs-cachefs-v0.3.0-linux-x86_64
sudo ./install.sh
```

### From Source

```bash
# Clone and build
git clone https://github.com/yourusername/nfs-cachefs.git
cd nfs-cachefs
git checkout v0.3.0
cargo build --release

# Install
sudo cp target/release/nfs-cachefs /usr/local/bin/
sudo ln -sf /usr/local/bin/nfs-cachefs /sbin/mount.cachefs
```

## Usage Examples

### Basic Mount
```bash
sudo mount -t cachefs cachefs /mnt/cached \
    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/nvme/cache,cache_size_gb=100,allow_other
```

### With Custom Options
```bash
sudo mount -t cachefs cachefs /mnt/cached \
    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/nvme/cache,cache_size_gb=200,block_size_mb=128,max_concurrent=20,allow_other
```

### Debug Mode (Foreground)
```bash
sudo mount -t cachefs cachefs /mnt/cached \
    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/nvme/cache,cache_size_gb=100,allow_other,foreground
```

## System Requirements

- Ubuntu 22.04 LTS or 24.04 LTS
- Linux Kernel 5.4+
- FUSE 3.0+
- x86_64 architecture

## Dependencies

The binary package includes all necessary dependencies except:
- libfuse3-3
- fuse3

Install them with:
```bash
sudo apt update
sudo apt install -y libfuse3-3 fuse3
```

## Known Issues

- None in this release

## Acknowledgments

Thanks to all contributors and users who reported issues and provided feedback.

## Support

For issues and questions:
- GitHub Issues: https://github.com/yourusername/nfs-cachefs/issues
- Documentation: https://github.com/yourusername/nfs-cachefs/wiki

## License

MIT License - see LICENSE file for details.