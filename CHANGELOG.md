# Changelog

All notable changes to this project will be documented in this file.

## [0.2.0] - 2025-01-09

### Fixed
- **Critical**: Fixed mount helper mode parameter parsing issue
  - Mount commands now correctly parse `-o` options when using `mount -t cachefs`
  - Fixed `nfs_backend` parameter not being read from mount options
  - Improved argument parsing logic for mount.cachefs helper mode

### Changed
- Enhanced mount helper detection and parameter processing
- Better error messages for mount parameter validation
- Improved support for standard mount command usage

### Technical Details
- Fixed loop logic in `parse_mount_helper_args()` function
- Corrected parameter extraction from `-o` option string
- Added proper support for comma-separated mount options
- Enhanced key-value pair parsing for mount parameters

### Usage
Mount commands now work correctly:
```bash
sudo mount -t cachefs cachefs /mnt/cached \
    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/cache,cache_size_gb=50,allow_other
```

## [0.1.0] - 2024-12-XX

### Added
- Initial release of NFS-CacheFS
- Read-only asynchronous caching filesystem for NFS
- FUSE-based implementation
- LRU cache eviction policy
- Configurable cache size and block size
- Support for concurrent caching operations