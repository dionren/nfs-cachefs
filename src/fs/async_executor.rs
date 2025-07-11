use std::sync::Arc;
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::cache::manager::CacheManager;
use crate::core::config::Config;
use crate::fs::inode::{InodeManager, FileAttr as InternalFileAttr, FileType as InternalFileType};

/// 异步操作请求
#[derive(Debug)]
pub enum AsyncRequest {
    Lookup {
        parent: u64,
        name: String,
        responder: oneshot::Sender<Result<InternalFileAttr, i32>>,
    },
    Read {
        ino: u64,
        offset: i64,
        size: u32,
        responder: oneshot::Sender<Result<Vec<u8>, i32>>,
    },
    Open {
        ino: u64,
        responder: oneshot::Sender<Result<u64, i32>>,
    },
    ReadDir {
        ino: u64,
        responder: oneshot::Sender<Result<Vec<(u64, String, fuser::FileType)>, i32>>,
    },
    Release {
        fh: u64,
        responder: oneshot::Sender<()>,
    },
}

/// 异步操作执行器
#[derive(Clone)]
pub struct AsyncExecutor {
    request_sender: mpsc::UnboundedSender<AsyncRequest>,
    _executor_handle: Arc<JoinHandle<()>>,
}

impl AsyncExecutor {
    pub fn new(
        config: Arc<Config>,
        inode_manager: Arc<InodeManager>,
        cache_manager: Arc<CacheManager>,
        open_files: Arc<tokio::sync::RwLock<std::collections::HashMap<u64, Arc<tokio::sync::RwLock<tokio::fs::File>>>>>,
        next_fh: Arc<tokio::sync::RwLock<u64>>,
    ) -> Self {
        let (request_sender, request_receiver) = mpsc::unbounded_channel();
        
        let executor_handle = tokio::spawn(Self::run_executor(
            config,
            inode_manager,
            cache_manager,
            open_files,
            next_fh,
            request_receiver,
        ));
        
        Self {
            request_sender,
            _executor_handle: Arc::new(executor_handle),
        }
    }
    
    /// 提交异步操作请求
    pub fn submit(&self, request: AsyncRequest) -> Result<(), mpsc::error::SendError<AsyncRequest>> {
        self.request_sender.send(request)
    }
    
    /// 执行器主循环
    async fn run_executor(
        config: Arc<Config>,
        inode_manager: Arc<InodeManager>,
        cache_manager: Arc<CacheManager>,
        open_files: Arc<tokio::sync::RwLock<std::collections::HashMap<u64, Arc<tokio::sync::RwLock<tokio::fs::File>>>>>,
        next_fh: Arc<tokio::sync::RwLock<u64>>,
        mut request_receiver: mpsc::UnboundedReceiver<AsyncRequest>,
    ) {
        while let Some(request) = request_receiver.recv().await {
            match request {
                AsyncRequest::Lookup { parent, name, responder } => {
                    let result = Self::handle_lookup(
                        &config,
                        &inode_manager,
                        parent,
                        &name,
                    ).await;
                    let _ = responder.send(result);
                }
                AsyncRequest::Read { ino, offset, size, responder } => {
                    let result = Self::handle_read(
                        &config,
                        &inode_manager,
                        &cache_manager,
                        ino,
                        offset,
                        size,
                    ).await;
                    let _ = responder.send(result);
                }
                AsyncRequest::Open { ino, responder } => {
                    let result = Self::handle_open(
                        &config,
                        &inode_manager,
                        &open_files,
                        &next_fh,
                        ino,
                    ).await;
                    let _ = responder.send(result);
                }
                AsyncRequest::ReadDir { ino, responder } => {
                    let result = Self::handle_readdir(
                        &config,
                        &inode_manager,
                        ino,
                    ).await;
                    let _ = responder.send(result);
                }
                AsyncRequest::Release { fh, responder } => {
                    Self::handle_release(&open_files, fh).await;
                    let _ = responder.send(());
                }
            }
        }
    }
    
    /// 处理 lookup 操作
    async fn handle_lookup(
        config: &Config,
        inode_manager: &InodeManager,
        parent: u64,
        name: &str,
    ) -> Result<InternalFileAttr, i32> {
        use libc::ENOENT;
        use fuser::FUSE_ROOT_ID;
        
        let parent_path = if parent == FUSE_ROOT_ID {
            PathBuf::from("/")
        } else {
            match inode_manager.get_path(parent) {
                Some(path) => path,
                None => return Err(ENOENT),
            }
        };
        
        let file_path = parent_path.join(name);
        let nfs_path = config.nfs_backend_path.join(file_path.strip_prefix("/").unwrap_or(&file_path));
        
        match tokio::fs::metadata(&nfs_path).await {
            Ok(metadata) => {
                let inode = inode_manager.get_inode(&file_path)
                    .unwrap_or_else(|| inode_manager.allocate_inode());
                
                let file_type = if metadata.is_dir() {
                    InternalFileType::Directory
                } else if metadata.is_file() {
                    InternalFileType::RegularFile
                } else {
                    InternalFileType::Symlink
                };
                
                let attr = InternalFileAttr {
                    inode,
                    size: metadata.len(),
                    blocks: (metadata.len() + 511) / 512,
                    atime: metadata.accessed().unwrap_or(SystemTime::now()),
                    mtime: metadata.modified().unwrap_or(SystemTime::now()),
                    ctime: metadata.created().unwrap_or(SystemTime::now()),
                    crtime: metadata.created().unwrap_or(SystemTime::now()),
                    kind: file_type,
                    perm: if metadata.is_dir() { 0o555 } else { 0o444 }, // 只读权限
                    nlink: 1,
                    uid: 1000,
                    gid: 1000,
                    rdev: 0,
                    flags: 0,
                };
                
                inode_manager.insert_mapping(file_path, inode, attr.clone());
                Ok(attr)
            }
            Err(_) => Err(ENOENT),
        }
    }
    
    /// 处理 read 操作 - 仅处理缓存文件读取
    async fn handle_read(
        config: &Config,
        inode_manager: &InodeManager,
        cache_manager: &CacheManager,
        ino: u64,
        offset: i64,
        size: u32,
    ) -> Result<Vec<u8>, i32> {
        use libc::ENOENT;
        
        let path = match inode_manager.get_path(ino) {
            Some(path) => path,
            None => return Err(ENOENT),
        };
        
        // 只处理缓存文件读取
        let cache_path = Self::get_cache_path(config, &path);
        debug!("Reading from cache: {}", cache_path.display());
        
        match Self::read_from_file(&cache_path, offset, size).await {
            Ok(data) => {
                cache_manager.record_access(&path);
                Ok(data)
            }
            Err(e) => {
                warn!("Failed to read from cache: {}", cache_path.display());
                Err(e)
            }
        }
    }
    
    /// 处理 open 操作 (只读模式)
    async fn handle_open(
        config: &Config,
        inode_manager: &InodeManager,
        open_files: &Arc<tokio::sync::RwLock<std::collections::HashMap<u64, Arc<tokio::sync::RwLock<tokio::fs::File>>>>>,
        next_fh: &Arc<tokio::sync::RwLock<u64>>,
        ino: u64,
    ) -> Result<u64, i32> {
        use libc::ENOENT;
        
        let path = match inode_manager.get_path(ino) {
            Some(path) => path,
            None => return Err(ENOENT),
        };
        
        let nfs_path = Self::get_nfs_path(config, &path);
        
        // 只读模式，仅以只读方式打开文件
        match tokio::fs::File::open(&nfs_path).await {
            Ok(file) => {
                let mut next = next_fh.write().await;
                let fh = *next;
                *next += 1;
                
                open_files.write().await.insert(fh, Arc::new(tokio::sync::RwLock::new(file)));
                Ok(fh)
            }
            Err(_) => Err(ENOENT),
        }
    }
    
    /// 处理 readdir 操作
    async fn handle_readdir(
        config: &Config,
        inode_manager: &InodeManager,
        ino: u64,
    ) -> Result<Vec<(u64, String, fuser::FileType)>, i32> {
        use libc::ENOENT;
        use fuser::FUSE_ROOT_ID;
        
        let path = if ino == FUSE_ROOT_ID {
            PathBuf::from("/")
        } else {
            match inode_manager.get_path(ino) {
                Some(path) => path,
                None => return Err(ENOENT),
            }
        };
        
        let nfs_path = Self::get_nfs_path(config, &path);
        
        let mut entries = match tokio::fs::read_dir(&nfs_path).await {
            Ok(entries) => entries,
            Err(_) => return Err(ENOENT),
        };
        
        let mut result = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let file_type = if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                fuser::FileType::Directory
            } else {
                fuser::FileType::RegularFile
            };
            
            let temp_ino = inode_manager.allocate_inode();
            result.push((temp_ino, name, file_type));
        }
        
        Ok(result)
    }
    
    /// 处理 release 操作
    async fn handle_release(
        open_files: &Arc<tokio::sync::RwLock<std::collections::HashMap<u64, Arc<tokio::sync::RwLock<tokio::fs::File>>>>>,
        fh: u64,
    ) {
        open_files.write().await.remove(&fh);
    }
    
    /// 辅助函数：从文件读取数据
    async fn read_from_file(file_path: &PathBuf, offset: i64, size: u32) -> Result<Vec<u8>, i32> {
        use libc::{ENOENT, EINVAL, EIO};
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        use std::io::SeekFrom;
        
        let mut file = match tokio::fs::File::open(file_path).await {
            Ok(f) => f,
            Err(_) => return Err(ENOENT),
        };
        
        if let Err(_) = file.seek(SeekFrom::Start(offset as u64)).await {
            return Err(EINVAL);
        }
        
        let mut buffer = vec![0; size as usize];
        match file.read(&mut buffer).await {
            Ok(bytes_read) => {
                buffer.truncate(bytes_read);
                Ok(buffer)
            }
            Err(_) => Err(EIO),
        }
    }
    
    /// 辅助函数：获取 NFS 路径
    fn get_nfs_path(config: &Config, path: &PathBuf) -> PathBuf {
        config.nfs_backend_path.join(path.strip_prefix("/").unwrap_or(path))
    }
    
    /// 辅助函数：获取缓存路径
    fn get_cache_path(config: &Config, path: &PathBuf) -> PathBuf {
        config.cache_dir.join(path.strip_prefix("/").unwrap_or(path))
    }
    
    /// 辅助函数：检查文件是否应该被缓存
    fn should_cache(_path: &PathBuf, size: u64, config: &Config) -> bool {
        // 检查文件大小是否超过缓存阈值
        if size < config.min_cache_file_size {
            return false;
        }
        
        // 检查文件是否太大，超过缓存总大小的10%
        let max_file_size = config.max_cache_size_bytes / 10;
        if size > max_file_size {
            return false;
        }
        
        // 所有满足大小条件的文件都可以缓存
        true
    }
} 