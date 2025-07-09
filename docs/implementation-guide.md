# NFS-CacheFS 实施指南

本文档提供了逐步实现NFS-CacheFS的详细指导。

## 第一阶段：项目初始化和基础框架

### 1.1 创建Rust项目

```bash
cargo new nfs-cachefs
cd nfs-cachefs
```

### 1.2 配置Cargo.toml

```toml
[package]
name = "nfs-cachefs"
version = "0.1.0"
edition = "2021"

[dependencies]
# FUSE绑定
fuser = "0.14"

# 异步运行时
tokio = { version = "1.35", features = ["full"] }

# LRU缓存
lru = "0.12"

# 日志
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# 错误处理
thiserror = "1.0"
anyhow = "1.0"

# 并发数据结构
dashmap = "5.5"
parking_lot = "0.12"

# 系统调用
nix = { version = "0.27", features = ["fs", "mount"] }
libc = "0.2"

# 工具
rand = "0.8"

[dev-dependencies]
tempfile = "3.8"
criterion = "0.5"

[[bench]]
name = "cache_benchmark"
harness = false
```

### 1.3 项目目录结构

```bash
mkdir -p src/{core,cache,fs,utils}
mkdir -p tests/{unit,integration}
mkdir -p benches
mkdir -p docs
```

### 1.4 基础模块定义

创建 `src/lib.rs`:

```rust
pub mod core;
pub mod cache;
pub mod fs;
pub mod utils;

pub use core::config::Config;
pub use fs::cachefs::CacheFs;
```

## 第二阶段：核心数据结构实现

### 2.1 配置结构 (`src/core/config.rs`)

```rust
use std::path::PathBuf;
use std::str::FromStr;
use anyhow::{Result, anyhow};

#[derive(Debug, Clone)]
pub struct Config {
    pub nfs_backend_path: PathBuf,
    pub cache_dir: PathBuf,
    pub mount_point: PathBuf,
    pub max_cache_size_bytes: u64,
    
    pub cache_block_size: usize,
    pub max_concurrent_caching: u32,
    pub enable_checksums: bool,
    pub cache_ttl_seconds: Option<u64>,
    pub direct_io: bool,
    pub readahead_bytes: usize,
    pub eviction_policy: EvictionPolicy,
}

#[derive(Debug, Clone)]
pub enum EvictionPolicy {
    Lru,
    Lfu,
    Arc,
}

impl FromStr for EvictionPolicy {
    type Err = anyhow::Error;
    
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "lru" => Ok(Self::Lru),
            "lfu" => Ok(Self::Lfu),
            "arc" => Ok(Self::Arc),
            _ => Err(anyhow!("Unknown eviction policy: {}", s)),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            nfs_backend_path: PathBuf::new(),
            cache_dir: PathBuf::new(),
            mount_point: PathBuf::new(),
            max_cache_size_bytes: 50 * 1024 * 1024 * 1024, // 50GB
            cache_block_size: 1024 * 1024, // 1MB
            max_concurrent_caching: 4,
            enable_checksums: false,
            cache_ttl_seconds: None,
            direct_io: true,
            readahead_bytes: 4 * 1024 * 1024, // 4MB
            eviction_policy: EvictionPolicy::Lru,
        }
    }
}

impl Config {
    /// 从 FUSE mount 选项解析配置
    pub fn from_mount_options(options: &[&str], mount_point: PathBuf) -> Result<Self> {
        let mut config = Config::default();
        config.mount_point = mount_point;
        
        for option in options {
            if let Some((key, value)) = option.split_once('=') {
                match key {
                    "nfs_backend" => config.nfs_backend_path = PathBuf::from(value),
                    "cache_dir" => config.cache_dir = PathBuf::from(value),
                    "cache_size_gb" => {
                        let gb: u64 = value.parse()?;
                        config.max_cache_size_bytes = gb * 1024 * 1024 * 1024;
                    }
                    "block_size_mb" => {
                        let mb: usize = value.parse()?;
                        config.cache_block_size = mb * 1024 * 1024;
                    }
                    "max_concurrent" => config.max_concurrent_caching = value.parse()?,
                    "checksum" => config.enable_checksums = value.parse()?,
                    "ttl_hours" => {
                        let hours: u64 = value.parse()?;
                        config.cache_ttl_seconds = Some(hours * 3600);
                    }
                    "direct_io" => config.direct_io = value.parse()?,
                    "readahead_mb" => {
                        let mb: usize = value.parse()?;
                        config.readahead_bytes = mb * 1024 * 1024;
                    }
                    "eviction" => config.eviction_policy = value.parse()?,
                    _ => {} // 忽略未知选项（如 allow_other 等 FUSE 标准选项）
                }
            }
        }
        
        // 验证必需参数
        if config.nfs_backend_path.as_os_str().is_empty() {
            return Err(anyhow!("Missing required option: nfs_backend"));
        }
        if config.cache_dir.as_os_str().is_empty() {
            return Err(anyhow!("Missing required option: cache_dir"));
        }
        
        Ok(config)
    }
}
```

### 2.2 缓存状态 (`src/cache/state.rs`)

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum CacheStatus {
    NotCached,
    CachingInProgress {
        started_at: Instant,
        progress: Arc<AtomicU64>,
    },
    Cached {
        cached_at: Instant,
        last_accessed: Instant,
    },
}

#[derive(Debug)]
pub struct CacheEntry {
    pub size: u64,
    pub status: CacheStatus,
    pub access_count: u64,
    pub checksum: Option<u64>,
}

impl CacheEntry {
    pub fn new(size: u64) -> Self {
        Self {
            size,
            status: CacheStatus::NotCached,
            access_count: 0,
            checksum: None,
        }
    }
    
    pub fn start_caching(&mut self) -> Arc<AtomicU64> {
        let progress = Arc::new(AtomicU64::new(0));
        self.status = CacheStatus::CachingInProgress {
            started_at: Instant::now(),
            progress: Arc::clone(&progress),
        };
        progress
    }
    
    pub fn complete_caching(&mut self) {
        self.status = CacheStatus::Cached {
            cached_at: Instant::now(),
            last_accessed: Instant::now(),
        };
    }
}
```

### 2.3 inode映射器 (`src/fs/inode_mapper.rs`)

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use parking_lot::RwLock;

pub struct InodeMapper {
    path_to_inode: RwLock<HashMap<PathBuf, u64>>,
    inode_to_path: RwLock<HashMap<u64, PathBuf>>,
    next_inode: AtomicU64,
}

impl InodeMapper {
    pub fn new() -> Self {
        Self {
            path_to_inode: RwLock::new(HashMap::new()),
            inode_to_path: RwLock::new(HashMap::new()),
            next_inode: AtomicU64::new(2), // 1 reserved for root
        }
    }
    
    pub fn get_or_create_inode(&self, path: PathBuf) -> u64 {
        // 先尝试读锁
        {
            let map = self.path_to_inode.read();
            if let Some(&inode) = map.get(&path) {
                return inode;
            }
        }
        
        // 需要创建新的inode
        let mut path_map = self.path_to_inode.write();
        let mut inode_map = self.inode_to_path.write();
        
        // 双重检查
        if let Some(&inode) = path_map.get(&path) {
            return inode;
        }
        
        let inode = self.next_inode.fetch_add(1, Ordering::SeqCst);
        path_map.insert(path.clone(), inode);
        inode_map.insert(inode, path);
        
        inode
    }
    
    pub fn get_path(&self, inode: u64) -> Option<PathBuf> {
        self.inode_to_path.read().get(&inode).cloned()
    }
}
```

## 第三阶段：FUSE文件系统实现

### 3.1 主文件系统结构 (`src/fs/cachefs.rs`)

```rust
use fuser::{
    Filesystem, Request, ReplyEntry, ReplyAttr, ReplyData, ReplyDirectory, 
    ReplyOpen, ReplyEmpty, FileType, FileAttr, FUSE_ROOT_ID
};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use lru::LruCache;
use parking_lot::Mutex;
use dashmap::DashMap;

use crate::core::config::Config;
use crate::cache::state::{CacheEntry, CacheStatus};
use crate::cache::manager::CacheManager;
use super::inode_mapper::InodeMapper;

pub struct CacheFs {
    config: Config,
    inode_mapper: Arc<InodeMapper>,
    cache_manager: Arc<CacheManager>,
    file_tracker: Arc<Mutex<LruCache<PathBuf, CacheEntry>>>,
    open_files: Arc<DashMap<u64, FileHandle>>,
}

struct FileHandle {
    path: PathBuf,
    flags: i32,
}

impl CacheFs {
    pub fn new(config: Config) -> Result<Self, anyhow::Error> {
        let cache_manager = Arc::new(CacheManager::new(&config)?);
        let max_entries = config.max_cache_size_bytes / (100 * 1024 * 1024); // 假设平均文件100MB
        
        Ok(Self {
            config,
            inode_mapper: Arc::new(InodeMapper::new()),
            cache_manager,
            file_tracker: Arc::new(Mutex::new(LruCache::new(max_entries as usize))),
            open_files: Arc::new(DashMap::new()),
        })
    }
    
    fn get_nfs_path(&self, relative_path: &PathBuf) -> PathBuf {
        self.config.nfs_backend_path.join(relative_path)
    }
    
    fn get_cache_path(&self, relative_path: &PathBuf) -> PathBuf {
        self.config.cache_dir.join(relative_path)
    }
}

impl Filesystem for CacheFs {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        // 实现文件查找逻辑
        todo!()
    }
    
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        // 实现获取文件属性
        todo!()
    }
    
    fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        // 实现文件打开逻辑，触发异步缓存
        todo!()
    }
    
    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        // 实现读取逻辑，根据缓存状态选择数据源
        todo!()
    }
    
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        reply: ReplyDirectory,
    ) {
        // 实现目录读取
        todo!()
    }
    
    fn release(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: u64,
        _flags: i32,
        _lock: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        // 清理文件句柄
        self.open_files.remove(&fh);
        reply.ok();
    }
}
```

## 第四阶段：异步缓存管理器

### 4.1 缓存管理器实现 (`src/cache/manager.rs`)

```rust
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use dashmap::DashMap;
use anyhow::Result;

use crate::core::config::Config;
use super::task::{CacheTask, CachePriority};

pub struct CacheManager {
    config: Config,
    semaphore: Arc<Semaphore>,
    task_sender: mpsc::Sender<CacheTask>,
    active_tasks: Arc<DashMap<PathBuf, JoinHandle<()>>>,
}

impl CacheManager {
    pub fn new(config: &Config) -> Result<Self> {
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_caching as usize));
        let (tx, rx) = mpsc::channel(1000);
        
        let manager = Self {
            config: config.clone(),
            semaphore,
            task_sender: tx,
            active_tasks: Arc::new(DashMap::new()),
        };
        
        // 启动任务处理器
        manager.start_task_processor(rx);
        
        Ok(manager)
    }
    
    pub async fn submit_cache_task(&self, source: PathBuf, target: PathBuf) -> Result<()> {
        let task = CacheTask {
            source_path: source,
            cache_path: target,
            priority: CachePriority::Normal,
            retry_count: 0,
        };
        
        self.task_sender.send(task).await?;
        Ok(())
    }
    
    fn start_task_processor(&self, mut receiver: mpsc::Receiver<CacheTask>) {
        let semaphore = Arc::clone(&self.semaphore);
        let active_tasks = Arc::clone(&self.active_tasks);
        
        tokio::spawn(async move {
            while let Some(task) = receiver.recv().await {
                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let tasks_ref = Arc::clone(&active_tasks);
                
                let handle = tokio::spawn(async move {
                    let _permit = permit; // 持有permit直到任务完成
                    
                    // 执行缓存复制
                    if let Err(e) = execute_cache_copy(&task).await {
                        tracing::error!("Cache task failed: {:?}", e);
                    }
                    
                    // 从活动任务中移除
                    tasks_ref.remove(&task.source_path);
                });
                
                active_tasks.insert(task.source_path.clone(), handle);
            }
        });
    }
}

async fn execute_cache_copy(task: &CacheTask) -> Result<()> {
    use tokio::fs;
    use tokio::io;
    
    // 确保目标目录存在
    if let Some(parent) = task.cache_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    
    // 使用临时文件
    let temp_path = task.cache_path.with_extension("caching");
    
    // 执行复制
    let mut source = fs::File::open(&task.source_path).await?;
    let mut target = fs::File::create(&temp_path).await?;
    
    io::copy(&mut source, &mut target).await?;
    
    // 确保数据写入磁盘
    target.sync_all().await?;
    
    // 原子重命名
    fs::rename(&temp_path, &task.cache_path).await?;
    
    tracing::info!("Cached file: {}", task.source_path.display());
    
    Ok(())
}
```

## 第五阶段：完整实现示例

### 5.1 完整的open实现

```rust
fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
    let path = match self.inode_mapper.get_path(ino) {
        Some(p) => p,
        None => {
            reply.error(libc::ENOENT);
            return;
        }
    };
    
    // 检查缓存状态
    let should_cache = {
        let mut tracker = self.file_tracker.lock();
        match tracker.get(&path) {
            None => {
                // 新文件，需要缓存
                let nfs_path = self.get_nfs_path(&path);
                if let Ok(metadata) = std::fs::metadata(&nfs_path) {
                    let entry = CacheEntry::new(metadata.len());
                    tracker.put(path.clone(), entry);
                    true
                } else {
                    false
                }
            }
            Some(entry) => matches!(entry.status, CacheStatus::NotCached),
        }
    };
    
    // 如果需要缓存，启动异步任务
    if should_cache {
        let source = self.get_nfs_path(&path);
        let target = self.get_cache_path(&path);
        let cache_manager = Arc::clone(&self.cache_manager);
        let file_tracker = Arc::clone(&self.file_tracker);
        let path_clone = path.clone();
        
        // 标记为正在缓存
        {
            let mut tracker = file_tracker.lock();
            if let Some(entry) = tracker.get_mut(&path) {
                entry.start_caching();
            }
        }
        
        // 启动异步缓存任务
        tokio::spawn(async move {
            if cache_manager.submit_cache_task(source, target).await.is_ok() {
                // 等待缓存完成并更新状态
                tokio::time::sleep(Duration::from_millis(100)).await;
                
                let mut tracker = file_tracker.lock();
                if let Some(entry) = tracker.get_mut(&path_clone) {
                    entry.complete_caching();
                }
            }
        });
    }
    
    // 创建文件句柄
    let fh = rand::random::<u64>();
    self.open_files.insert(fh, FileHandle { path, flags });
    
    reply.opened(fh, 0);
}
```

### 5.2 完整的read实现

```rust
fn read(
    &mut self,
    _req: &Request,
    _ino: u64,
    fh: u64,
    offset: i64,
    size: u32,
    _flags: i32,
    _lock: Option<u64>,
    reply: ReplyData,
) {
    let handle = match self.open_files.get(&fh) {
        Some(h) => h,
        None => {
            reply.error(libc::EBADF);
            return;
        }
    };
    
    let path = handle.path.clone();
    
    // 检查缓存状态
    let cache_status = {
        let tracker = self.file_tracker.lock();
        tracker.get(&path).map(|e| e.status.clone())
    };
    
    let file_path = match cache_status {
        Some(CacheStatus::Cached { .. }) => {
            tracing::debug!("Reading from cache: {}", path.display());
            self.get_cache_path(&path)
        }
        _ => {
            tracing::debug!("Reading from NFS: {}", path.display());
            self.get_nfs_path(&path)
        }
    };
    
    // 执行读取
    use std::os::unix::fs::FileExt;
    match std::fs::File::open(&file_path) {
        Ok(file) => {
            let mut buffer = vec![0u8; size as usize];
            match file.read_at(&mut buffer, offset as u64) {
                Ok(n) => {
                    buffer.truncate(n);
                    reply.data(&buffer);
                }
                Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
            }
        }
        Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
    }
}
```

## 第六阶段：测试实现

### 6.1 单元测试示例 (`tests/unit/cache_state_test.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_cache_entry_state_transitions() {
        let mut entry = CacheEntry::new(1024);
        
        // 初始状态
        assert!(matches!(entry.status, CacheStatus::NotCached));
        
        // 开始缓存
        let progress = entry.start_caching();
        assert!(matches!(entry.status, CacheStatus::CachingInProgress { .. }));
        
        // 更新进度
        progress.store(512, Ordering::SeqCst);
        
        // 完成缓存
        entry.complete_caching();
        assert!(matches!(entry.status, CacheStatus::Cached { .. }));
    }
}
```

### 6.2 集成测试示例 (`tests/integration/basic_operations.rs`)

```rust
use tempfile::TempDir;
use std::fs;
use std::path::Path;

#[tokio::test]
async fn test_file_caching() {
    // 创建临时目录
    let nfs_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();
    let mount_dir = TempDir::new().unwrap();
    
    // 创建测试文件
    let test_file = nfs_dir.path().join("test.txt");
    fs::write(&test_file, b"Hello, World!").unwrap();
    
    // 配置文件系统
    let config = Config {
        nfs_backend_path: nfs_dir.path().to_path_buf(),
        cache_dir: cache_dir.path().to_path_buf(),
        mount_point: mount_dir.path().to_path_buf(),
        max_cache_size_bytes: 1024 * 1024 * 1024, // 1GB
        ..Default::default()
    };
    
    // 创建并挂载文件系统
    let fs = CacheFs::new(config).unwrap();
    
    // TODO: 实际挂载和测试操作
}
```

## 第七阶段：部署和运维

### 7.1 主程序入口 (`src/main.rs`)

```rust
use fuser::MountOption;
use std::env;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// 标准 mount helper 程序格式：
/// mount.cachefs <source> <mount_point> -o <options>
fn main() -> anyhow::Result<()> {
    // 初始化日志
    let log_level = env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();
    
    // 解析命令行参数
    let args: Vec<String> = env::args().collect();
    
    // 支持两种调用方式：
    // 1. 作为 mount helper: mount.cachefs source mountpoint -o options
    // 2. 直接调用: cachefs mountpoint -o options
    let (mount_point, options_str) = if args.len() >= 4 && args[3] == "-o" {
        // mount helper 方式
        (PathBuf::from(&args[2]), args.get(4).cloned())
    } else if args.len() >= 3 && args[1] == "-o" {
        // 直接调用方式（用于测试）
        (PathBuf::from(&args[0]), args.get(2).cloned())
    } else {
        eprintln!("Usage: {} <mount_point> -o <options>", args[0]);
        eprintln!("或作为 mount helper: mount.cachefs <source> <mount_point> -o <options>");
        std::process::exit(1);
    };
    
    // 解析挂载选项
    let options_str = options_str.unwrap_or_default();
    let mount_options: Vec<&str> = options_str.split(',').collect();
    
    // 创建配置
    let config = nfs_cachefs::Config::from_mount_options(&mount_options, mount_point.clone())?;
    
    tracing::info!("Starting CacheFS with config: {:?}", config);
    
    // 创建文件系统
    let fs = nfs_cachefs::CacheFs::new(config)?;
    
    // 构建 FUSE 挂载选项
    let mut fuse_options = vec![
        MountOption::FSName("cachefs".to_string()),
        MountOption::Subtype("cachefs".to_string()),
    ];
    
    // 解析 FUSE 相关选项
    for option in mount_options {
        match option {
            "allow_other" => fuse_options.push(MountOption::AllowOther),
            "allow_root" => fuse_options.push(MountOption::AllowRoot),
            "auto_unmount" => fuse_options.push(MountOption::AutoUnmount),
            "default_permissions" => fuse_options.push(MountOption::DefaultPermissions),
            "nodev" => fuse_options.push(MountOption::NoDev),
            "nosuid" => fuse_options.push(MountOption::NoSuid),
            "ro" => fuse_options.push(MountOption::RO),
            "noatime" => fuse_options.push(MountOption::NoAtime),
            "sync" => fuse_options.push(MountOption::Sync),
            _ => {} // 忽略非 FUSE 选项
        }
    }
    
    // 挂载文件系统
    tracing::info!("Mounting CacheFS at: {}", mount_point.display());
    fuser::mount2(fs, &mount_point, &fuse_options)?;
    
    Ok(())
}
```

### 7.2 创建 mount helper 链接

为了让系统能够识别 `cachefs` 文件系统类型，需要创建一个符号链接：

```bash
# 编译程序
cargo build --release

# 安装二进制文件
sudo cp target/release/nfs-cachefs /usr/local/bin/

# 创建 mount helper 链接
sudo ln -s /usr/local/bin/nfs-cachefs /sbin/mount.cachefs

# 验证安装
mount.cachefs --version
```

### 7.2 性能监控端点

在实际部署中，可以添加一个HTTP端点来暴露Prometheus指标：

```rust
use axum::{routing::get, Router};
use prometheus::{Encoder, TextEncoder};

async fn metrics_handler() -> String {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

pub fn start_metrics_server() {
    let app = Router::new().route("/metrics", get(metrics_handler));
    
    tokio::spawn(async {
        axum::Server::bind(&"0.0.0.0:9100".parse().unwrap())
            .serve(app.into_make_service())
            .await
            .unwrap();
    });
}
```

## 最佳实践和注意事项

1. **错误处理**：始终优雅地处理错误，降级到NFS访问
2. **并发控制**：限制同时进行的缓存操作数量
3. **空间管理**：实现智能的LRU驱逐策略
4. **监控告警**：设置关键指标的告警阈值
5. **安全考虑**：确保缓存文件的权限与原文件一致

## 下一步

完成基础实现后，可以考虑以下高级特性：

1. 智能预取机制
2. 压缩支持
3. 加密支持
4. 分布式缓存协调
5. Web管理界面

本指南将随着项目进展持续更新。 