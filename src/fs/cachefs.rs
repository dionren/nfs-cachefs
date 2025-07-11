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
        
        Ok(Self {
            inode_manager,
            async_executor,
            cache_manager: Arc::clone(&cache_manager),
            config: Arc::new(config),
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
        use std::fs::File;
        use std::io::{Read, Seek, SeekFrom};
        
        // 对于大文件使用更大的读取缓冲区
        let buffer_size = if size > 1024 * 1024 { // 1MB以上
            std::cmp::min(size as usize, 16 * 1024 * 1024) // 最大16MB
        } else {
            size as usize
        };
        
        let mut file = match File::open(cache_path) {
            Ok(f) => f,
            Err(_) => return Err(libc::ENOENT),
        };
        
        if let Err(_) = file.seek(SeekFrom::Start(offset as u64)) {
            return Err(libc::EINVAL);
        }
        
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
    
    /// 直接同步读取NFS文件
    fn read_nfs_direct(file_path: &std::path::PathBuf, offset: i64, size: u32) -> Result<Vec<u8>, i32> {
        use std::io::{Read, Seek, SeekFrom};
        use std::fs::File;
        
        let mut file = match File::open(file_path) {
            Ok(f) => f,
            Err(_) => return Err(libc::ENOENT),
        };
        
        if let Err(_) = file.seek(SeekFrom::Start(offset as u64)) {
            return Err(libc::EINVAL);
        }
        
        let mut buffer = vec![0; size as usize];
        match file.read(&mut buffer) {
            Ok(bytes_read) => {
                buffer.truncate(bytes_read);
                Ok(buffer)
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
            
            match Self::read_cache_smart(&cache_path, offset, size) {
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
                
                // 异步触发缓存任务（仅对大文件）
                let cache_manager = Arc::clone(&self.cache_manager);
                let nfs_path_clone = nfs_path.clone();
                let path_clone = path.clone();
                tokio::spawn(async move {
                    if let Ok(metadata) = tokio::fs::metadata(&nfs_path_clone).await {
                        if metadata.len() > 1024 * 1024 { // 1MB以上才缓存
                            let file_size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
                            tracing::info!("🔄 CACHE TRIGGER: {} ({:.1}MB) -> starting background cache", 
                                path_clone.display(), file_size_mb);
                                
                            if let Err(e) = cache_manager.submit_cache_task(
                                nfs_path_clone, 
                                crate::cache::state::CachePriority::Normal
                            ).await {
                                tracing::warn!("❌ CACHE TASK FAILED: {}: {}", path_clone.display(), e);
                            }
                        } else {
                            let file_size_kb = metadata.len() as f64 / 1024.0;
                            tracing::debug!("⏭️  CACHE SKIP: {} ({:.1}KB) -> too small for caching", 
                                path_clone.display(), file_size_kb);
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