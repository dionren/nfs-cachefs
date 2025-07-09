# NFS-CacheFS: 高性能异步缓存文件系统

## 项目概述

NFS-CacheFS 是一个基于 FUSE 的高性能异步缓存文件系统，专门设计用于加速 NFS 上大型文件（如深度学习模型）的访问。通过将文件异步缓存到本地 NVMe SSD，实现对应用程序完全透明的加速。

### 背景与需求

- **场景**：在深度学习训练环境中，需要频繁从NFS读取大型模型文件（10-100GB），网络带宽成为瓶颈
- **目标**：利用本地NVMe SSD作为透明缓存层，加速文件访问，同时保持对应用完全透明
- **挑战**：传统缓存方案在首次读取时会因为缓存填充而导致延迟

### 核心创新

1. **零延迟首次访问**：首次读取直接穿透到 NFS，不会被缓存操作阻塞
2. **异步后台缓存**：在后台独立线程中完成文件复制，不影响前台操作
3. **原子性保证**：使用临时文件+rename确保缓存文件始终完整
4. **智能缓存管理**：基于 LRU 的自动驱逐策略，高效利用有限的 SSD 空间

## 技术架构

### 系统架构

```
┌─────────────────┐
│   应用程序      │
│  (PyTorch等)    │
└────────┬────────┘
         │ 文件操作
         ▼
┌─────────────────┐
│   CacheFS       │
│  (FUSE层)       │
├─────────────────┤
│ • 请求路由      │
│ • 状态管理      │
│ • 异步调度      │
└────┬────────┬───┘
     │        │
     ▼        ▼
┌────────┐ ┌────────┐
│  NFS   │ │ NVMe   │
│ Backend│ │ Cache  │
└────────┘ └────────┘
```

### 文件缓存状态机

```
NotCached ──────┐
    │           │
    │ open()    │
    ▼           │
CachingInProgress─┤
    │           │
    │ 复制完成  │
    ▼           │
Cached ─────────┘
    │
    │ LRU驱逐
    ▼
NotCached
```

### 技术栈

- **编程语言**: Rust (性能和内存安全)
- **核心框架**: FUSE (用户态文件系统)
- **异步运行时**: Tokio
- **主要依赖**:
  - `fuser`: Rust FUSE 绑定
  - `lru`: LRU 缓存实现
  - `dashmap`: 并发安全的 HashMap
  - `tracing`: 结构化日志
  - `parking_lot`: 高性能同步原语

## 关键设计

### 1. 核心数据结构

```rust
// 文件缓存状态
#[derive(Debug, Clone, PartialEq)]
enum CacheStatus {
    NotCached,
    CachingInProgress {
        started_at: Instant,
        progress: Arc<AtomicU64>, // 已复制字节数
    },
    Cached {
        cached_at: Instant,
        last_accessed: Instant,
    },
}

// 缓存条目
struct CacheEntry {
    size: u64,
    status: CacheStatus,
    access_count: u64,
    checksum: Option<u64>, // 用于验证数据完整性
}

// 文件系统配置 - 从fstab挂载选项解析
#[derive(Debug, Clone)]
struct CacheFsConfig {
    nfs_backend_path: PathBuf,
    cache_dir: PathBuf,
    max_cache_size_gb: u64,
    
    // 高级配置（带默认值）
    cache_block_size: usize,    // 默认: 1MB
    max_concurrent_caching: u32, // 默认: 4
    cache_eviction_policy: EvictionPolicy, // 默认: LRU
    enable_checksum: bool,       // 默认: false
    cache_ttl_hours: Option<u64>, // 默认: 无限制
    direct_io: bool,            // 默认: true
    readahead_mb: u32,          // 默认: 4MB
}
```

### 2. 配置解析

配置直接从 fstab 挂载选项解析，无需额外配置文件：

```rust
impl CacheFsConfig {
    fn from_mount_options(options: &str) -> Result<Self> {
        let mut config = Self::default();
        
        for opt in options.split(',') {
            let parts: Vec<&str> = opt.split('=').collect();
            match parts[0] {
                "nfs_backend" => config.nfs_backend_path = PathBuf::from(parts[1]),
                "cache_dir" => config.cache_dir = PathBuf::from(parts[1]),
                "cache_size_gb" => config.max_cache_size_gb = parts[1].parse()?,
                // ... 其他选项解析
            }
        }
        Ok(config)
    }
}
```

### 3. 关键设计决策

1. **异步缓存而非同步缓存**
   - 避免首次访问延迟
   - 提供更好的用户体验
   - 后台任务独立管理

2. **用户态文件系统（FUSE）**
   - 无需修改内核
   - 易于部署和维护
   - 对应用程序完全透明

3. **Rust 语言选择**
   - 高性能，接近 C/C++
   - 内存安全，避免常见错误
   - 优秀的并发支持

4. **LRU 缓存驱逐**
   - 简单有效的缓存管理策略
   - 可扩展为更复杂的算法（LFU、ARC）

## 核心模块设计

### 模块1: 文件系统层 (`src/fs.rs`)

实现 FUSE 接口，处理所有文件系统操作：

```rust
impl Filesystem for CacheFs {
    fn lookup(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry);
    fn getattr(&mut self, req: &Request, ino: u64, reply: ReplyAttr);
    fn open(&mut self, req: &Request, ino: u64, flags: i32, reply: ReplyOpen);
    fn read(&mut self, req: &Request, ino: u64, fh: u64, offset: i64, size: u32, flags: i32, lock: Option<u64>, reply: ReplyData);
    fn readdir(&mut self, req: &Request, ino: u64, fh: u64, offset: i64, reply: ReplyDirectory);
    fn release(&mut self, req: &Request, ino: u64, fh: u64, flags: i32, lock: Option<u64>, flush: bool, reply: ReplyEmpty);
}
```

### 模块2: 异步缓存管理器 (`src/cache_manager.rs`)

管理后台缓存任务，控制并发和资源使用：

```rust
struct CacheManager {
    pending_tasks: Arc<Mutex<VecDeque<CacheTask>>>,
    active_tasks: Arc<DashMap<PathBuf, JoinHandle<()>>>,
    semaphore: Arc<Semaphore>, // 限制并发缓存任务数
    stats: Arc<CacheStats>,
}

impl CacheManager {
    async fn submit_cache_task(&self, task: CacheTask) -> Result<()>;
    async fn execute_cache_copy(&self, task: &CacheTask) -> Result<()>;
    async fn verify_cache_integrity(&self, path: &Path) -> Result<bool>;
}
```

### 模块3: 缓存驱逐策略 (`src/eviction.rs`)

实现可扩展的缓存驱逐机制：

```rust
trait EvictionPolicy: Send + Sync {
    fn should_evict(&self, entry: &CacheEntry, cache_pressure: f64) -> bool;
    fn select_victims(&self, entries: &[CacheEntry], bytes_needed: u64) -> Vec<PathBuf>;
}

struct AdaptiveLRU {
    age_weight: f64,
    frequency_weight: f64,
    size_weight: f64,
}
```

### 模块4: 监控与诊断 (`src/metrics.rs`)

提供详细的性能指标和诊断信息：

```rust
struct CacheStats {
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    bytes_served_from_cache: AtomicU64,
    bytes_served_from_nfs: AtomicU64,
    ongoing_cache_operations: AtomicU32,
    completed_cache_operations: AtomicU64,
    failed_cache_operations: AtomicU64,
    read_latency_histogram: Mutex<Histogram>,
}
```

## 性能优化

### 1. 零拷贝优化

使用 Linux 特定的系统调用减少数据复制：

```rust
#[cfg(target_os = "linux")]
fn zero_copy_transfer(src_fd: RawFd, dst_fd: RawFd, len: usize) -> io::Result<usize> {
    use nix::fcntl::{splice, SpliceFFlags};
    // 使用 splice 实现零拷贝
}
```

### 2. 内存映射优化

对大文件使用 mmap 提高性能：

```rust
struct MmapCache {
    cached_mappings: LruCache<PathBuf, Arc<Mmap>>,
    max_mapped_files: usize,
}
```

### 3. 预读取优化

智能预取机制提高顺序访问性能：

```rust
struct PrefetchPredictor {
    access_history: LruCache<PathBuf, Vec<Instant>>,
    directory_patterns: HashMap<PathBuf, AccessPattern>,
}
```

## 错误处理与容错

### 错误分层

```rust
#[derive(Debug, thiserror::Error)]
enum CacheFsError {
    #[error("NFS backend error: {0}")]
    NfsError(#[source] io::Error),
    
    #[error("Cache operation failed: {0}")]
    CacheError(String),
    
    #[error("Insufficient cache space")]
    InsufficientSpace,
    
    #[error("Checksum mismatch for file: {path}")]
    ChecksumMismatch { path: PathBuf },
}
```

### 容错机制

1. **部分缓存恢复**：系统重启后自动扫描并恢复未完成的缓存操作
2. **损坏检测**：定期校验缓存文件完整性
3. **自动清理**：清理损坏或过期的缓存条目
4. **优雅降级**：缓存失败时自动降级到直接 NFS 访问

## 部署配置

### 安装步骤

```bash
# 编译程序
cargo build --release

# 安装二进制文件
sudo cp target/release/nfs-cachefs /usr/local/bin/

# 创建 mount helper 链接（重要！）
sudo ln -s /usr/local/bin/nfs-cachefs /sbin/mount.cachefs
```

### fstab 配置

```fstab
# /etc/fstab 配置示例

# 1. 挂载 NFS（必须在 CacheFS 之前）
10.20.66.201:/share    /mnt/nfs    nfs    defaults,_netdev    0 0

# 2. 挂载本地缓存磁盘
/dev/nvme0n1    /mnt/nvme    xfs    defaults,noatime    0 0

# 3. 挂载 CacheFS
cachefs    /mnt/cached    cachefs    nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,cache_size_gb=50,direct_io=true,allow_other,_netdev    0 0
```

### 配置参数说明

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `nfs_backend` | 必需 | NFS 后端挂载路径 |
| `cache_dir` | 必需 | 本地缓存目录路径 |
| `cache_size_gb` | 必需 | 最大缓存大小（GB） |
| `block_size_mb` | 1 | 缓存块大小（MB） |
| `max_concurrent` | 4 | 最大并发缓存任务数 |
| `eviction` | lru | 缓存驱逐策略（lru/lfu/arc） |
| `direct_io` | true | 是否使用直接I/O |
| `readahead_mb` | 4 | 预读取大小（MB） |

## 性能指标

基于设计和类似系统的经验，预期性能提升：

| 指标 | 提升幅度 | 说明 |
|------|----------|------|
| 顺序读取 | 10-20x | 取决于网络速度 |
| 随机访问 | 50-100x | NVMe延迟远低于网络 |
| 延迟 | 100-1000x | 本地访问 vs 网络访问 |
| 缓存命中率 | >90% | 稳定工作负载下 |

## 运维管理

### 日常运维

```bash
# 查看缓存状态
nfs-cachefs-ctl status

# 清理过期缓存
nfs-cachefs-ctl cache clean --older-than 7d

# 预热重要文件
nfs-cachefs-ctl cache warm /path/to/models/

# 导出性能指标
nfs-cachefs-ctl metrics export
```

### 监控集成

支持 Prometheus 指标导出：

```yaml
scrape_configs:
  - job_name: 'nfs-cachefs'
    static_configs:
      - targets: ['localhost:9100']
    metrics_path: '/metrics'
```

### 故障排查

```bash
# 启用调试日志
RUST_LOG=debug mount -t cachefs ...

# 分析缓存命中率
journalctl -t cachefs | grep "CACHE_HIT\|CACHE_MISS"

# 检查健康状态
nfs-cachefs-ctl health check
```

## 项目路线图

### v1.0 - 基础功能（当前）
- ✅ 基本 FUSE 文件系统
- ✅ 异步缓存机制
- ✅ LRU 驱逐策略
- ✅ 基础监控指标
- ✅ fstab 集成

### v2.0 - 高级特性
- ⏳ 智能预取（基于访问模式）
- ⏳ 压缩支持（透明压缩/解压）
- ⏳ 分布式缓存协调
- ⏳ 缓存预热工具

### v3.0 - 企业特性
- ⏳ 多级缓存（RAM → NVMe → HDD）
- ⏳ S3/对象存储支持
- ⏳ Web 管理界面
- ⏳ 缓存加密

## 项目价值

1. **显著性能提升**：对于大文件密集型应用，可实现 10-100 倍性能提升
2. **完全透明**：无需修改任何应用程序代码
3. **易于部署**：通过标准的 fstab 配置即可使用，无需额外配置文件
4. **高可靠性**：设计中考虑了各种故障场景
5. **资源高效**：智能缓存管理，最大化利用有限的 SSD 空间

## 相关文档

- [实施指南](implementation-guide.md) - 详细的开发实施步骤
- [测试计划](testing-plan.md) - 全面的测试策略
- [fstab配置指南](fstab-configuration.md) - 配置参数详解

## 参考资源

- [FUSE Documentation](https://libfuse.github.io/doxygen/)
- [Rust Async Book](https://rust-lang.github.io/async-book/)
- [Linux Kernel Caching](https://www.kernel.org/doc/html/latest/filesystems/caching.html)
- 类似项目：[CacheFS](https://github.com/kahing/catfs), [S3FS](https://github.com/s3fs-fuse/s3fs-fuse), [JuiceFS](https://github.com/juicedata/juicefs)

---

**注意**：本文档会随着项目进展持续更新。最新版本请查看项目仓库。 