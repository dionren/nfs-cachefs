# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Development Commands

### Building
```bash
# Production build using Docker (recommended)
make build

# Local development build
cargo build --release

# Clean build artifacts
make clean
```

### Testing
```bash
# Run all tests
cargo test

# Run tests in Docker container
make test

# Run specific test module
cargo test cache::tests

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

### Development Tools
```bash
# Watch for changes and rebuild
cargo install cargo-watch
cargo watch -x check -x test

# View expanded macros
cargo install cargo-expand
cargo expand
```

## Architecture Overview

NFS-CacheFS is a high-performance, read-only FUSE filesystem that provides transparent caching for NFS-mounted files. The system is designed around performance optimization for large file access scenarios.

### Core Architecture
- **FUSE Layer** (`src/fs/cachefs.rs`): Main filesystem implementation handling all FUSE operations
- **Cache Manager** (`src/cache/manager.rs`): Central orchestration of cache operations and state management
- **Async Executor** (`src/fs/async_executor.rs`): Handles asynchronous FUSE operations using Tokio
- **Configuration System** (`src/core/config.rs`): Comprehensive configuration management with performance tuning options

### Key Design Principles
1. **Read-Only Design**: Filesystem is explicitly read-only for safety and performance
2. **Zero-Copy Optimization**: Large files (>1GB) use zero-copy reads to minimize memory overhead
3. **Intelligent Caching**: Files are cached asynchronously in the background while serving reads from NFS
4. **High Concurrency**: Built on Tokio async runtime with configurable concurrency limits

### Data Flow
1. Application requests file read
2. CacheFS checks cache status in `CacheManager`
3. If cached: Direct read from local NVMe storage
4. If not cached: Read from NFS backend while triggering background cache task
5. Background task copies file to cache directory atomically
6. Subsequent reads served from high-speed local cache

## Key Components

### Cache State Machine
Cache entries follow a strict state machine pattern (`src/cache/state.rs`):
- `NotCached` → `CachingInProgress` → `Cached`
- `Failed` state for error handling
- Atomic state transitions with progress tracking

### Eviction Policies
Pluggable eviction system (`src/cache/eviction.rs`):
- LRU (default): Least Recently Used
- LFU: Least Frequently Used  
- ARC: Adaptive Replacement Cache
- Configurable via `eviction` mount option

### Performance Optimizations
- **Large Block Sizes**: 4MB default blocks for better throughput
- **NVMe Optimizations**: io_uring support, configurable queue depths
- **Smart Caching**: Minimum file size thresholds, streaming vs caching decisions
- **Direct I/O**: Bypasses OS page cache when beneficial

## Configuration

### Required Mount Options
- `nfs_backend=/path/to/nfs` - NFS backend mount point
- `cache_dir=/path/to/cache` - Local cache directory (preferably NVMe)
- `cache_size_gb=N` - Maximum cache size in GB

### Performance Tuning Options
- `block_size_mb=4` - Cache block size (1-64MB)
- `max_concurrent=8` - Maximum concurrent cache tasks
- `readahead_mb=16` - Read-ahead buffer size
- `direct_io=true` - Enable direct I/O
- `zero_copy_threshold_gb=1` - Use zero-copy for files larger than threshold

### Advanced Options
- `eviction=lru` - Cache eviction policy (lru/lfu/arc)
- `checksum=true` - Enable file integrity checksums
- `ttl_hours=24` - Cache entry time-to-live
- `min_cache_file_mb=10` - Minimum file size to cache

## Working with the Codebase

### Adding New Features
1. **Cache Policies**: Implement `EvictionPolicy` trait in `src/cache/eviction.rs`
2. **Configuration Options**: Add to `Config` struct in `src/core/config.rs`
3. **FUSE Operations**: Extend `CacheFs` implementation in `src/fs/cachefs.rs`
4. **Metrics**: Add new metrics in `src/cache/metrics.rs`

### Error Handling
All errors use the centralized error system in `src/core/error.rs`:
- `CacheError` for cache-related issues
- `NfsError` for NFS backend problems
- `ConfigError` for configuration validation
- Recovery strategies: retry, fallback, fail

### Testing Approach
- Unit tests for individual modules
- Integration tests require NFS backend setup
- Performance benchmarks using `criterion` crate
- Mock filesystems for testing cache behavior

### Key Constants
- `DEFAULT_BLOCK_SIZE`: 4MB for optimal throughput
- `DEFAULT_READAHEAD_SIZE`: 8MB for sequential access
- `ZERO_COPY_THRESHOLD`: 1GB for memory efficiency
- `MAX_CONCURRENT_TASKS`: 8 for balanced performance

### Thread Safety
- Use `Arc<DashMap<>>` for shared concurrent state
- `parking_lot` mutexes for low-latency locking
- Atomic operations for counters and flags
- Channel-based communication between async tasks

## Deployment Considerations

### Performance Requirements
- NVMe storage strongly recommended for cache directory
- Minimum 8GB RAM for metadata caching
- Stable network connection to NFS backend

### Mount Dependencies
Always ensure proper mount order in `/etc/fstab`:
1. NFS backend mount
2. Local cache filesystem mount  
3. CacheFS mount with `_netdev` option

### Monitoring
- Cache hit/miss ratios in logs
- Read latency metrics via tracing
- Storage utilization monitoring
- Eviction frequency tracking

## Security Notes

This is a defensive security tool designed for read-only file caching. The filesystem:
- Never modifies source NFS data
- Validates all mount options and paths
- Implements secure temporary file handling
- Uses atomic operations for cache consistency