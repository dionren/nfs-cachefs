use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, Request, FUSE_ROOT_ID,
};
use libc::ENOENT;
use tokio::sync::RwLock;
use tokio::sync::oneshot;
use tracing::error;

use crate::cache::manager::CacheManager;
use crate::core::config::Config;
use crate::fs::inode::InodeManager;
use crate::fs::async_executor::{AsyncExecutor, AsyncRequest};

#[cfg(feature = "io_uring")]
use crate::io::{IoUringExecutor, IoUringConfig};

/// NFS-CacheFS 只读文件系统实现
#[derive(Clone)]
pub struct CacheFs {
    /// inode 管理器
    inode_manager: Arc<InodeManager>,
    /// 异步操作执行器
    async_executor: AsyncExecutor,
    /// 缓存管理器
    cache_manager: Arc<CacheManager>,
    /// 配置信息
    config: Arc<Config>,
    /// io_uring 执行器 (可选)
    #[cfg(feature = "io_uring")]
    io_uring_executor: Option<Arc<IoUringExecutor>>,
}

impl CacheFs {
    /// 创建新的 CacheFS 实例
    pub fn new(config: Config) -> tokio::io::Result<Self> {
        let inode_manager = Arc::new(InodeManager::new());
        let metrics = Arc::new(crate::cache::metrics::MetricsCollector::new());
        let cache_manager = Arc::new(CacheManager::new(Arc::new(config.clone()), metrics)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?);
        
        let open_files = Arc::new(RwLock::new(HashMap::new()));
        let next_fh = Arc::new(RwLock::new(1));
        
        let async_executor = AsyncExecutor::new(
            Arc::new(config.clone()),
            Arc::clone(&inode_manager),
            Arc::clone(&cache_manager),
            Arc::clone(&open_files),
            Arc::clone(&next_fh),
        );
        
        // Initialize io_uring executor if enabled
        #[cfg(feature = "io_uring")]
        let io_uring_executor = if config.nvme.use_io_uring {
            tracing::info!("Initializing io_uring support...");
            
            // Check kernel support
            if !crate::io::check_io_uring_support() {
                tracing::warn!("io_uring not supported on this system, falling back to traditional I/O");
                None
            } else {
                let io_config = IoUringConfig {
                    queue_depth: config.nvme.queue_depth,
                    sq_poll: config.nvme.polling_mode,
                    io_poll: config.nvme.io_poll,
                    fixed_buffers: config.nvme.fixed_buffers,
                    huge_pages: config.nvme.use_hugepages,
                    sq_poll_idle: config.nvme.sq_poll_idle_ms,
                };
                
                match IoUringExecutor::new(io_config) {
                    Ok(executor) => {
                        tracing::info!("✅ io_uring initialized successfully");
                        Some(Arc::new(executor))
                    }
                    Err(e) => {
                        tracing::error!("Failed to initialize io_uring: {}", e);
                        None
                    }
                }
            }
        } else {
            None
        };
        
        Ok(Self {
            inode_manager,
            async_executor,
            cache_manager: Arc::clone(&cache_manager),
            config: Arc::new(config),
            #[cfg(feature = "io_uring")]
            io_uring_executor,
        })
    }
    
    /// 优雅关闭文件系统
    pub async fn shutdown(&self) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!("Shutting down CacheFS...");
        
        // 关闭缓存管理器
        if let Err(e) = self.cache_manager.shutdown().await {
            tracing::warn!("Error shutting down cache manager: {}", e);
        }
        
        tracing::info!("CacheFS shutdown completed");
        Ok(())
    }
    
    /// 获取缓存路径的辅助函数
    fn get_cache_path(&self, path: &std::path::PathBuf) -> std::path::PathBuf {
        self.config.cache_dir.join(path.strip_prefix("/").unwrap_or(path))
    }
    
    /// 获取NFS路径
    fn get_nfs_path(&self, path: &std::path::PathBuf) -> std::path::PathBuf {
        self.config.nfs_backend_path.join(path.strip_prefix("/").unwrap_or(path))
    }
    
    /// 直接同步读取缓存文件 - 优化版本
    fn read_cache_direct(cache_path: &std::path::PathBuf, offset: i64, size: u32) -> Result<Vec<u8>, i32> {
        use std::io::{Read, Seek, SeekFrom};
        use std::fs::File;
        
        let mut file = match File::open(cache_path) {
            Ok(f) => f,
            Err(_) => return Err(libc::ENOENT),
        };
        
        if let Err(_) = file.seek(SeekFrom::Start(offset as u64)) {
            return Err(libc::EINVAL);
        }
        
        let mut buffer = vec![0u8; size as usize];
        match file.read(&mut buffer) {
            Ok(bytes_read) => {
                buffer.truncate(bytes_read);
                Ok(buffer)
            }
            Err(_) => Err(libc::EIO),
        }
    }
    
    /// 零拷贝穿透读取 - 针对大文件的优化
    fn read_cache_zero_copy(cache_path: &std::path::PathBuf, offset: i64, size: u32) -> Result<Vec<u8>, i32> {
        use std::fs::{File, OpenOptions};
        use std::io::{Read, Seek, SeekFrom};
        use std::os::unix::fs::OpenOptionsExt;
        
        // 对于大文件使用更大的读取缓冲区
        let buffer_size = if size > 64 * 1024 * 1024 { // 64MB以上
            std::cmp::min(size as usize, 256 * 1024 * 1024) // 最大256MB
        } else if size > 1024 * 1024 { // 1MB以上
            std::cmp::min(size as usize, 64 * 1024 * 1024) // 最大64MB
        } else {
            size as usize
        };
        
        // 使用 O_DIRECT 打开文件以绕过内核缓存
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT)
            .open(cache_path)
        {
            Ok(f) => f,
            Err(_) => {
                // 如果 O_DIRECT 失败，回退到普通打开
                match File::open(cache_path) {
                    Ok(f) => f,
                    Err(_) => return Err(libc::ENOENT),
                }
            }
        };
        
        if let Err(_) = file.seek(SeekFrom::Start(offset as u64)) {
            return Err(libc::EINVAL);
        }
        
        // 使用对齐的缓冲区以支持 O_DIRECT
        let mut buffer = vec![0u8; buffer_size];
        let mut total_read = 0;
        let mut result = Vec::with_capacity(size as usize);
        
        while total_read < size as usize {
            let remaining = size as usize - total_read;
            let read_size = std::cmp::min(buffer_size, remaining);
            
            match file.read(&mut buffer[..read_size]) {
                Ok(0) => break, // EOF
                Ok(bytes_read) => {
                    result.extend_from_slice(&buffer[..bytes_read]);
                    total_read += bytes_read;
                }
                Err(_) => return Err(libc::EIO),
            }
        }
        
        Ok(result)
    }
    
    /// 智能缓存读取 - 根据文件大小选择最优读取策略
    fn read_cache_smart(cache_path: &std::path::PathBuf, offset: i64, size: u32) -> Result<Vec<u8>, i32> {
        // 小文件使用直接读取
        if size < 4 * 1024 * 1024 { // 4MB以下
            Self::read_cache_direct(cache_path, offset, size)
        } else {
            // 大文件使用零拷贝穿透读取
            Self::read_cache_zero_copy(cache_path, offset, size)
        }
    }
    
    /// 使用 io_uring 读取缓存文件
    #[cfg(feature = "io_uring")]
    async fn read_cache_io_uring(
        io_uring_executor: &IoUringExecutor,
        cache_path: &std::path::PathBuf,
        offset: i64,
        size: u32,
    ) -> Result<Vec<u8>, i32> {
        match io_uring_executor.read_direct(cache_path, offset as u64, size).await {
            Ok(data) => Ok(data),
            Err(e) => {
                tracing::warn!("io_uring read failed: {}, falling back to traditional I/O", e);
                Err(libc::EIO)
            }
        }
    }
    
    /// 直接同步读取NFS文件 - 优化版本
    fn read_nfs_direct(file_path: &std::path::PathBuf, offset: i64, size: u32) -> Result<Vec<u8>, i32> {
        use std::io::{Read, Seek, SeekFrom, BufReader};
        use std::fs::File;
        
        let mut file = match File::open(file_path) {
            Ok(f) => f,
            Err(_) => return Err(libc::ENOENT),
        };
        
        if let Err(_) = file.seek(SeekFrom::Start(offset as u64)) {
            return Err(libc::EINVAL);
        }
        
        // 使用更大的缓冲读取器优化NFS读取
        let reader_capacity = if size > 16 * 1024 * 1024 { // 16MB以上
            16 * 1024 * 1024 // 16MB缓冲
        } else if size > 1024 * 1024 { // 1MB以上
            4 * 1024 * 1024 // 4MB缓冲
        } else {
            size as usize // 小文件直接读取
        };
        
        let mut reader = BufReader::with_capacity(reader_capacity, file);
        let mut buffer = vec![0; size as usize];
        
        match reader.read_exact(&mut buffer) {
            Ok(_) => Ok(buffer),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // 处理文件末尾的情况
                let mut partial_buffer = vec![0; size as usize];
                match reader.read(&mut partial_buffer) {
                    Ok(bytes_read) => {
                        partial_buffer.truncate(bytes_read);
                        Ok(partial_buffer)
                    }
                    Err(_) => Err(libc::EIO),
                }
            }
            Err(_) => Err(libc::EIO),
        }
    }
}

impl Filesystem for CacheFs {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let (sender, receiver) = oneshot::channel();
        let name = name.to_string_lossy().to_string();
        
        let request = AsyncRequest::Lookup {
            parent,
            name,
            responder: sender,
        };
        
        if let Err(e) = self.async_executor.submit(request) {
            error!("Failed to submit lookup request: {}", e);
            reply.error(libc::EIO);
            return;
        }
        
        // 等待异步操作完成
        tokio::spawn(async move {
            match receiver.await {
                Ok(Ok(attr)) => {
                    let fuse_attr: FileAttr = attr.into();
                    reply.entry(&Duration::from_secs(1), &fuse_attr, 0);
                }
                Ok(Err(err)) => reply.error(err),
                Err(_) => reply.error(libc::EIO),
            }
        });
    }
    
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let inode_manager = Arc::clone(&self.inode_manager);
        
        if let Some(attr) = inode_manager.get_attr(ino) {
            let fuse_attr: FileAttr = attr.into();
            reply.attr(&Duration::from_secs(1), &fuse_attr);
        } else {
            reply.error(ENOENT);
        }
    }
    
    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        let start_time = std::time::Instant::now();
        
        // 获取文件路径
        let path = match self.inode_manager.get_path(ino) {
            Some(path) => path,
            None => {
                tracing::error!("❌ File not found: inode={}", ino);
                reply.error(libc::ENOENT);
                return;
            }
        };
        
        let cache_path = self.get_cache_path(&path);
        let file_size_str = if size > 1024 * 1024 {
            format!("{:.1}MB", size as f64 / (1024.0 * 1024.0))
        } else if size > 1024 {
            format!("{:.1}KB", size as f64 / 1024.0)
        } else {
            format!("{}B", size)
        };
        
        tracing::info!("📁 READ REQUEST: {} (offset: {}, size: {})", 
            path.display(), offset, file_size_str);
        
        // 优化：缓存命中时直接同步读取，避免异步开销
        if std::fs::metadata(&cache_path).is_ok() {
            tracing::info!("🚀 CACHE HIT: {}", path.display());
            let cache_start = std::time::Instant::now();
            
            // Try io_uring first if available
            #[cfg(feature = "io_uring")]
            let read_result = if let Some(ref io_uring_exec) = self.io_uring_executor {
                tracing::debug!("Using io_uring for cache read");
                // Convert async io_uring read to sync using blocking task
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        Self::read_cache_io_uring(io_uring_exec, &cache_path, offset, size).await
                    })
                })
            } else {
                Self::read_cache_smart(&cache_path, offset, size)
            };
            
            #[cfg(not(feature = "io_uring"))]
            let read_result = Self::read_cache_smart(&cache_path, offset, size);
            
            match read_result {
                Ok(data) => {
                    let cache_duration = cache_start.elapsed();
                    let total_duration = start_time.elapsed();
                    let speed_mbps = if cache_duration.as_secs_f64() > 0.0 {
                        (data.len() as f64) / (1024.0 * 1024.0) / cache_duration.as_secs_f64()
                    } else {
                        0.0
                    };
                    
                    // 记录访问统计
                    self.cache_manager.record_access(&path);
                    
                    tracing::info!("✅ CACHE READ SUCCESS: {} -> {} in {:?} ({:.1} MB/s, total: {:?})", 
                        path.display(), 
                        file_size_str,
                        cache_duration,
                        speed_mbps,
                        total_duration
                    );
                    
                    reply.data(&data);
                    return;
                }
                Err(err) => {
                    let cache_duration = cache_start.elapsed();
                    // 缓存读取失败，降级到NFS
                    tracing::warn!("⚠️  CACHE READ FAILED: {} -> falling back to NFS (error: {}, time: {:?})", 
                        cache_path.display(), err, cache_duration);
                }
            }
        } else {
            tracing::info!("❌ CACHE MISS: {} -> reading from NFS", path.display());
        }
        
        // 缓存未命中或读取失败，直接同步读取NFS
        let nfs_path = self.get_nfs_path(&path);
        let nfs_start = std::time::Instant::now();
        
        tracing::info!("🌐 NFS READ: {} (offset: {}, size: {})", 
            nfs_path.display(), offset, file_size_str);
            
        match Self::read_nfs_direct(&nfs_path, offset, size) {
            Ok(data) => {
                let nfs_duration = nfs_start.elapsed();
                let total_duration = start_time.elapsed();
                let speed_mbps = if nfs_duration.as_secs_f64() > 0.0 {
                    (data.len() as f64) / (1024.0 * 1024.0) / nfs_duration.as_secs_f64()
                } else {
                    0.0
                };
                
                tracing::info!("✅ NFS READ SUCCESS: {} -> {} in {:?} ({:.1} MB/s, total: {:?})", 
                    path.display(), 
                    file_size_str,
                    nfs_duration,
                    speed_mbps,
                    total_duration
                );
                
                reply.data(&data);
                
                // 异步触发缓存任务（仅对大文件），延迟执行避免与读取竞争
                let cache_manager = Arc::clone(&self.cache_manager);
                let nfs_path_clone = nfs_path.clone();
                let path_clone = path.clone();
                let config = Arc::clone(&self.config);
                tokio::spawn(async move {
                    // 延迟缓存任务，让用户读取优先完成
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    
                    if let Ok(metadata) = tokio::fs::metadata(&nfs_path_clone).await {
                        let min_cache_size = config.min_cache_file_size * 1024 * 1024; // MB转字节
                        if metadata.len() >= min_cache_size {
                            let file_size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
                            tracing::info!("🔄 CACHE TRIGGER (DELAYED): {} ({:.1}MB) -> starting background cache", 
                                path_clone.display(), file_size_mb);
                                
                            // 使用低优先级缓存任务
                            if let Err(e) = cache_manager.submit_cache_task(
                                nfs_path_clone, 
                                crate::cache::state::CachePriority::Low
                            ).await {
                                tracing::warn!("❌ CACHE TASK FAILED: {}: {}", path_clone.display(), e);
                            }
                        } else {
                            let file_size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
                            tracing::debug!("⏭️  CACHE SKIP: {} ({:.1}MB) -> below minimum size ({} MB)", 
                                path_clone.display(), file_size_mb, config.min_cache_file_size);
                        }
                    }
                });
            }
            Err(err) => {
                let nfs_duration = nfs_start.elapsed();
                let total_duration = start_time.elapsed();
                tracing::error!("❌ NFS READ FAILED: {} -> error {} (nfs: {:?}, total: {:?})", 
                    path.display(), err, nfs_duration, total_duration);
                reply.error(err);
            }
        }
    }
    
    fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        let (sender, receiver) = oneshot::channel();
        
        let request = AsyncRequest::Open {
            ino,
            responder: sender,
        };
        
        if let Err(e) = self.async_executor.submit(request) {
            error!("Failed to submit open request: {}", e);
            reply.error(libc::EIO);
            return;
        }
        
        tokio::spawn(async move {
            match receiver.await {
                Ok(Ok(fh)) => reply.opened(fh, 0),
                Ok(Err(err)) => reply.error(err),
                Err(_) => reply.error(libc::EIO),
            }
        });
    }
    
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let (sender, receiver) = oneshot::channel();
        
        let request = AsyncRequest::ReadDir {
            ino,
            responder: sender,
        };
        
        if let Err(e) = self.async_executor.submit(request) {
            error!("Failed to submit readdir request: {}", e);
            reply.error(libc::EIO);
            return;
        }
        
        tokio::spawn(async move {
            match receiver.await {
                Ok(Ok(entries)) => {
                    let mut index = 0;
                    
                    // 添加 . 和 .. 条目
                    if offset <= index {
                        if reply.add(ino, index + 1, FileType::Directory, ".") {
                            reply.ok();
                            return;
                        }
                    }
                    index += 1;
                    
                    if offset <= index {
                        let parent_ino = if ino == FUSE_ROOT_ID {
                            FUSE_ROOT_ID
                        } else {
                            FUSE_ROOT_ID
                        };
                        
                        if reply.add(parent_ino, index + 1, FileType::Directory, "..") {
                            reply.ok();
                            return;
                        }
                    }
                    index += 1;
                    
                    // 添加实际的目录条目
                    for (temp_ino, name, file_type) in entries {
                        if offset <= index {
                            if reply.add(temp_ino, index + 1, file_type, &name) {
                                break;
                            }
                        }
                        index += 1;
                    }
                    
                    reply.ok();
                }
                Ok(Err(err)) => reply.error(err),
                Err(_) => reply.error(libc::EIO),
            }
        });
    }
    
    fn release(
        &mut self,
        _req: &Request,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        let (sender, receiver) = oneshot::channel();
        
        let request = AsyncRequest::Release {
            fh,
            responder: sender,
        };
        
        if let Err(e) = self.async_executor.submit(request) {
            error!("Failed to submit release request: {}", e);
            reply.error(libc::EIO);
            return;
        }
        
        tokio::spawn(async move {
            let _ = receiver.await;
            reply.ok();
        });
    }
} 