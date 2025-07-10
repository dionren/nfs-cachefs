# Changelog

All notable changes to this project will be documented in this file.

## [0.3.0] - 2025-01-10

### Added
- Automatic daemonization support for mount helper mode
- Background running by default when invoked via mount command
- `foreground` mount option to disable automatic daemonization
- Comprehensive mount troubleshooting documentation

### Fixed
- Mount command hanging issue - now properly forks to background
- FUSE mount options handling for better compatibility
- Signal handling for graceful shutdown

### Changed
- Mount helper now runs in background by default
- Improved logging with thread IDs for better debugging
- Enhanced error messages for mount failures

### Documentation
- Added `MOUNT_SOLUTION.md` with detailed mounting instructions
- Added `NFS_CACHEFS_TROUBLESHOOTING.md` for common issues
- Updated README with clearer mount examples

## [0.2.0] - 2024-12-10

### Added
- Read-only filesystem mode (removed write support)
- Improved cache management with LRU eviction
- Better error handling and recovery

### Changed
- Filesystem is now read-only by default
- File permissions set to 0o444 (read-only)
- Directory permissions set to 0o555

## [0.1.0] - 2024-11-15

### Added
- Initial release
- Basic NFS caching functionality
- Asynchronous cache filling
- FUSE filesystem implementation