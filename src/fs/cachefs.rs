use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, ReplyWrite, Request, FUSE_ROOT_ID,
};
use libc::ENOENT;
use tokio::sync::RwLock;
use tokio::sync::oneshot;
use tracing::error;

use crate::cache::manager::CacheManager;
use crate::core::config::Config;
use crate::fs::inode::InodeManager;
use crate::fs::async_executor::{AsyncExecutor, AsyncRequest};

/// NFS-CacheFS 文件系统实现
pub struct CacheFs {
    /// 配置信息
    config: Config,
    /// inode 管理器
    inode_manager: Arc<InodeManager>,
    /// 缓存管理器
    cache_manager: Arc<CacheManager>,
    /// 打开的文件句柄
    open_files: Arc<RwLock<HashMap<u64, Arc<RwLock<tokio::fs::File>>>>>,
    /// 下一个文件句柄
    next_fh: Arc<RwLock<u64>>,
    /// 异步操作执行器
    async_executor: AsyncExecutor,
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
            config,
            inode_manager,
            cache_manager,
            open_files,
            next_fh,
            async_executor,
        })
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
        let (sender, receiver) = oneshot::channel();
        
        let request = AsyncRequest::Read {
            ino,
            offset,
            size,
            responder: sender,
        };
        
        if let Err(e) = self.async_executor.submit(request) {
            error!("Failed to submit read request: {}", e);
            reply.error(libc::EIO);
            return;
        }
        
        tokio::spawn(async move {
            match receiver.await {
                Ok(Ok(data)) => reply.data(&data),
                Ok(Err(err)) => reply.error(err),
                Err(_) => reply.error(libc::EIO),
            }
        });
    }
    
    fn write(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let (sender, receiver) = oneshot::channel();
        let data = data.to_vec();
        
        let request = AsyncRequest::Write {
            ino,
            offset,
            data,
            responder: sender,
        };
        
        if let Err(e) = self.async_executor.submit(request) {
            error!("Failed to submit write request: {}", e);
            reply.error(libc::EIO);
            return;
        }
        
        tokio::spawn(async move {
            match receiver.await {
                Ok(Ok(bytes_written)) => reply.written(bytes_written),
                Ok(Err(err)) => reply.error(err),
                Err(_) => reply.error(libc::EIO),
            }
        });
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