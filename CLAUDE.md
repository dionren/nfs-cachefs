# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Development Commands

### Building
```bash
# Production build
cargo build --release

# Build with io_uring support (optional, Linux 5.10+)
cargo build --release --features io_uring

# Clean build artifacts
cargo clean
```

### Testing
```bash
# Run all tests
cargo test

# Run specific test module
cargo test cache::tests
cargo test fs::inode::tests

# Run integration tests (requires test environment)
cargo test --test integration
```

### Code Quality
```bash
# Format code
cargo fmt

# Run linter
cargo clippy

# Check code without building
cargo check
```

## Architecture Overview

NFS-CacheFS is a high-performance, read-only FUSE filesystem that provides transparent caching for NFS-mounted files. The system is designed around performance optimization for large file access scenarios.

### Core Architecture
- **FUSE Layer** (`src/fs/cachefs.rs`): Main filesystem implementation. All FUSE callbacks execute synchronously in fuser's callback threads — no async bridging or channel dispatch. Background cache tasks are spawned via a stored `tokio::runtime::Handle`.
- **Inode Manager** (`src/fs/inode.rs`): Thread-safe inode allocation and path↔inode↔attr mapping using `parking_lot::RwLock`.
- **Cache Manager** (`src/cache/manager.rs`): Central orchestration of cache operations, eviction, task scheduling, and metrics.
- **Configuration System** (`src/core/config.rs`): Configuration management with mount option parsing and validation.

### Key Design Principles
1. **Synchronous FUSE Callbacks**: All FUSE operations (`lookup`, `getattr`, `read`, `readdir`, `open`, `release`, `readlink`, `statfs`, `access`) complete synchronously within the callback thread. This avoids the pitfall of spawning async tasks that outlive the FUSE reply lifetime.
2. **Read-Only Design**: Filesystem is explicitly read-only for safety and performance. Write operations return `EROFS`.
3. **Background Caching**: On cache miss, data is served directly from NFS. A background tokio task then copies the file to local cache for future reads.
4. **Fork-Before-Threads**: In daemon mode, `fork()` runs before the Tokio runtime is created, avoiding undefined behavior from forking a multi-threaded process.
5. **Consistent Inodes**: `readdir` and `lookup` share a unified inode allocation via `InodeManager::get_or_allocate_inode()`, ensuring the kernel sees consistent inode numbers.

### Data Flow
1. Application requests file read via FUSE
2. CacheFS checks if a valid cache file exists locally (with mtime-based invalidation)
3. If cached and valid: direct synchronous read from local storage
4. If not cached: synchronous read from NFS backend, reply immediately, then spawn background cache task
5. Background task copies file to cache directory atomically (temp file + rename)
6. Subsequent reads served from high-speed local cache

### Runtime Architecture
```
Main Thread                  Tokio Runtime (background)
─────────────               ──────────────────────────
parse_args()
daemonize() [if daemon mode]
Runtime::new()  ──────────→  worker threads start
CacheFs::new(handle)
signal handler spawn  ────→  SIGINT/SIGTERM → fusermount -u
fuser::mount2() [blocks]     cache task processor
  ├─ lookup()   [sync]       cache copy tasks (tokio::spawn)
  ├─ getattr()  [sync]       eviction / cleanup
  ├─ read()     [sync]
  ├─ readdir()  [sync]
  └─ ...
[unmount returns]
runtime.block_on(shutdown)
runtime.shutdown_timeout(5s)
```

## Key Components

### Cache State Machine
Cache entries follow a strict state machine pattern (`src/cache/state.rs`):
- `NotCached` → `CachingInProgress` → `Cached`
- `Failed` state for error handling with retry support
- `Cached` state stores `source_mtime` for invalidation checks
- Atomic state transitions with progress tracking

### Cache Invalidation
- Each `CacheStatus::Cached` entry stores the source file's `mtime` at cache time
- On cache hit in `read()`, the source NFS file's current `mtime` is compared with the stored value
- Mismatched mtime triggers cache invalidation (file removed, re-fetched from NFS)
- Optional TTL-based expiration via `cache_ttl_seconds` config

### Eviction Policies
Pluggable eviction system (`src/cache/eviction.rs`):
- LRU (default): Least Recently Used
- LFU: Least Frequently Used
- ARC: Adaptive Replacement Cache
- Configurable via `eviction` mount option

### Inode Management
`InodeManager` (`src/fs/inode.rs`) provides:
- `get_or_allocate_inode(path)`: Returns existing inode or allocates a new one (used by both `lookup` and `readdir` for consistency)
- `insert_mapping(path, inode, attr)`: Stores full path↔inode↔attr triple
- Thread-safe with `parking_lot::RwLock` (read-heavy, low contention)

### Metadata Handling
- File attributes (uid, gid, mode, nlink, blocks, times) are read from the real NFS backend via `std::os::unix::fs::MetadataExt`
- `symlink_metadata()` is used in `lookup` and `readdir` to correctly identify symlinks
- Attributes are cached in InodeManager with FUSE TTL of 10 seconds
- `open()` refreshes attributes from NFS to detect changes

### Supported FUSE Operations
| Operation | Behavior |
|-----------|----------|
| `lookup` | Stat NFS file, allocate/reuse inode, cache attrs |
| `getattr` | Return cached attrs, fallback to NFS stat |
| `read` | Cache hit → local read; miss → NFS read + background cache |
| `readdir` | Read NFS directory, create consistent inode mappings |
| `open` | Verify file exists, refresh attrs, allocate file handle |
| `release` | No-op (read-only filesystem) |
| `readlink` | Read symlink target from NFS backend |
| `statfs` | Proxy NFS backend filesystem statistics |
| `access` | Allow read/execute, deny write (`EROFS`) |

## Configuration

### CLI Usage
```bash
nfs-cachefs <nfs_backend> <mountpoint> [OPTIONS]

# Example
nfs-cachefs /mnt/nfs /mnt/cache-mount \
  --cache-dir /fast-nvme/cache \
  --cache-size 100 \
  --min-cache-file-size 50 \
  -f  # foreground mode
```

### Mount Helper Mode
```bash
mount -t cachefs none /mnt/target -o nfs_backend=/mnt/nfs,cache_dir=/fast/cache,cache_size_gb=50
```

### Performance Tuning Options
- `block_size_mb=64` - Cache block size for large file copies (default: 64)
- `max_concurrent=10` - Maximum concurrent background cache tasks (default: 10)
- `min_cache_file_size_mb=100` - Minimum file size to trigger caching (default: 100)
- `cache_size_gb=10` - Maximum total cache size (default: 10)

### Advanced Options
- `eviction=lru` - Cache eviction policy (lru/lfu/arc)
- `checksum=true` - Enable SHA-256 file integrity checksums
- `ttl_hours=24` - Cache entry time-to-live
- `allow_other` - Allow other users to access the mount
- `foreground` / `fg` - Run in foreground (don't daemonize)

## Working with the Codebase

### Adding New Features
1. **Cache Policies**: Implement `EvictionPolicy` trait in `src/cache/eviction.rs`
2. **Configuration Options**: Add to `Config` struct in `src/core/config.rs`, update `from_mount_options` and both arg parsers in `main.rs`
3. **FUSE Operations**: Add methods to `impl Filesystem for CacheFs` in `src/fs/cachefs.rs` — keep them synchronous
4. **Metrics**: Add new counters/gauges in `src/cache/metrics.rs`

### Important Conventions
- **FUSE callbacks must be synchronous**: Never use `tokio::spawn` to dispatch work that owns a FUSE reply object. The reply must be sent before the callback returns.
- **Use `get_or_allocate_inode`**: When creating inode mappings in both `readdir` and `lookup`, always use `InodeManager::get_or_allocate_inode()` to maintain consistency.
- **`min_cache_file_size` is in bytes**: The config field is already converted from MB. Do not multiply again.
- **Cache entries are keyed by cache_path**: Methods like `record_access`, `is_cached`, `get_entry` expect the cache directory path, not the logical mount path.
- **Fork safety**: Any daemonization must happen before `Runtime::new()`. Never fork after creating a Tokio runtime.

### Error Handling
All errors use the centralized error system in `src/core/error.rs`:
- `CacheError` for cache-related issues
- `NfsError` for NFS backend problems
- `ConfigError` for configuration validation
- `InsufficientSpace` for cache capacity issues
- Recovery strategies: retry with exponential backoff, fallback to NFS, fail

### Thread Safety
- `Arc<DashMap<>>` for concurrent cache entry state
- `parking_lot::RwLock` for inode mappings (read-heavy)
- `parking_lot::Mutex` for eviction policy (write-heavy)
- `AtomicU64` for counters (metrics, file handles, inode allocation)
- Tokio `Semaphore` for concurrency limiting on cache tasks

### Key Constants (`src/lib.rs`)
- `DEFAULT_CACHE_BLOCK_SIZE`: 64MB for large file cache copies
- `DEFAULT_MAX_CONCURRENT`: 10 concurrent background cache tasks
- `DEFAULT_READAHEAD_SIZE`: 32MB for sequential access optimization

## Deployment

### Requirements
- Linux with FUSE support (`libfuse3-dev` / `fuse3`)
- Rust toolchain for building
- NFS backend must be mounted before CacheFS
- Cache directory on fast local storage (NVMe recommended)

### Mount Order in `/etc/fstab`
1. NFS backend mount
2. CacheFS mount with `_netdev` option

### Monitoring
- Cache hit/miss ratios via `tracing` logs (INFO level)
- `MetricsCollector` tracks: hit rate, latency, throughput, error counts
- Circular buffers for latency stats (bounded memory)
- Historical snapshots via `PerformanceMonitor`

## Security Notes

This is a read-only caching filesystem. It:
- Never modifies source NFS data
- Validates all mount options and paths
- Uses atomic file operations (temp + rename) for cache consistency
- Propagates real file ownership (uid/gid/mode) from NFS backend
- Denies all write operations with `EROFS`
