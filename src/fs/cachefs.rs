use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, ReplyWrite, Request, FUSE_ROOT_ID,
};
use libc::ENOENT;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::cache::manager::CacheManager;
use crate::cache::state::CachePriority;
use crate::core::config::Config;
use crate::fs::inode::{InodeManager, FileAttr as InternalFileAttr, FileType as InternalFileType};

/// NFS-CacheFS 文件系统实现
pub struct CacheFs {
    /// 配置信息
    config: Config,
    /// inode 管理器
    inode_manager: Arc<InodeManager>,
    /// 缓存管理器
    cache_manager: Arc<CacheManager>,
    /// 打开的文件句柄
    open_files: Arc<RwLock<HashMap<u64, Arc<RwLock<File>>>>>,
    /// 下一个文件句柄
    next_fh: Arc<RwLock<u64>>,
}

impl CacheFs {
    /// 创建新的 CacheFS 实例
    pub fn new(config: Config) -> tokio::io::Result<Self> {
        let inode_manager = Arc::new(InodeManager::new());
        let metrics = Arc::new(crate::cache::metrics::MetricsCollector::new());
        let cache_manager = Arc::new(CacheManager::new(Arc::new(config.clone()), metrics)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?);
        
        Ok(Self {
            config,
            inode_manager,
            cache_manager,
            open_files: Arc::new(RwLock::new(HashMap::new())),
            next_fh: Arc::new(RwLock::new(1)),
        })
    }
    
    /// 分配新的文件句柄
    async fn allocate_fh(&self) -> u64 {
        let mut next = self.next_fh.write().await;
        let fh = *next;
        *next += 1;
        fh
    }
    
    /// 获取 NFS 后端的完整路径
    fn get_nfs_path(&self, path: &Path) -> PathBuf {
        self.config.nfs_backend_path.join(path.strip_prefix("/").unwrap_or(path))
    }
    
    /// 获取缓存文件的完整路径
    fn get_cache_path(&self, path: &Path) -> PathBuf {
        self.config.cache_dir.join(path.strip_prefix("/").unwrap_or(path))
    }
    
    /// 从 NFS 后端获取文件属性
    async fn get_nfs_attr(&self, path: &Path) -> Result<InternalFileAttr, i32> {
        let nfs_path = self.get_nfs_path(path);
        
        match std::fs::metadata(&nfs_path) {
            Ok(metadata) => {
                let inode = self.inode_manager.get_inode(path)
                    .unwrap_or_else(|| self.inode_manager.allocate_inode());
                
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
                    perm: 0o644, // 简化的权限处理
                    nlink: 1,
                    uid: 1000,
                    gid: 1000,
                    rdev: 0,
                    flags: 0,
                };
                
                self.inode_manager.insert_mapping(path.to_path_buf(), inode, attr.clone());
                Ok(attr)
            }
            Err(_) => Err(ENOENT),
        }
    }
    
    /// 检查文件是否应该被缓存
    fn should_cache(&self, path: &Path, size: u64) -> bool {
        // 只缓存普通文件
        if let Some(attr) = self.inode_manager.get_attr(
            self.inode_manager.get_inode(path).unwrap_or(0)
        ) {
            if attr.kind != InternalFileType::RegularFile {
                return false;
            }
        }
        
        // 检查文件大小限制
        let max_file_size = self.config.max_cache_size_bytes / 10; // 最大文件不超过缓存大小的10%
        if size > max_file_size {
            return false;
        }
        
        // 检查文件扩展名（可选的过滤逻辑）
        if let Some(ext) = path.extension() {
            let ext_str = ext.to_string_lossy().to_lowercase();
            // 缓存常见的大文件类型
            matches!(ext_str.as_str(), "bin" | "model" | "weights" | "data" | "db" | "tar" | "zip" | "gz")
        } else {
            // 没有扩展名的文件也可以缓存
            true
        }
    }
    
    /// 触发异步缓存
    async fn trigger_cache(&self, path: &Path, priority: CachePriority) {
        if let Err(e) = self.cache_manager.submit_cache_task(path.to_path_buf(), priority).await {
            warn!("Failed to trigger cache for {}: {}", path.display(), e);
        }
    }
    
    /// 从缓存或 NFS 读取文件数据
    async fn read_file_data(&self, path: &Path, offset: i64, size: u32) -> Result<Vec<u8>, i32> {
        // 首先尝试从缓存读取
        let cache_path = self.get_cache_path(path);
        if cache_path.exists() {
            debug!("Reading from cache: {}", cache_path.display());
            match self.read_from_file(&cache_path, offset, size).await {
                Ok(data) => {
                    // 更新缓存访问统计
                    self.cache_manager.record_access(&path.to_path_buf());
                    return Ok(data);
                }
                Err(e) => {
                    warn!("Failed to read from cache {}: {}", cache_path.display(), e);
                    // 缓存文件损坏，删除它
                    let _ = std::fs::remove_file(&cache_path);
                }
            }
        }
        
        // 从 NFS 后端读取
        let nfs_path = self.get_nfs_path(path);
        debug!("Reading from NFS: {}", nfs_path.display());
        
        match self.read_from_file(&nfs_path, offset, size).await {
            Ok(data) => {
                // 记录缓存未命中
                self.cache_manager.record_access(&path.to_path_buf());
                
                // 如果文件应该被缓存，触发异步缓存
                if let Ok(metadata) = std::fs::metadata(&nfs_path) {
                    if self.should_cache(path, metadata.len()) {
                        self.trigger_cache(path, CachePriority::Normal).await;
                    }
                }
                
                Ok(data)
            }
            Err(e) => Err(e),
        }
    }
    
    /// 从指定文件读取数据
    async fn read_from_file(&self, file_path: &Path, offset: i64, size: u32) -> Result<Vec<u8>, i32> {
        let mut file = match File::open(file_path) {
            Ok(f) => f,
            Err(_) => return Err(ENOENT),
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
    
    /// 写入文件数据到 NFS 后端
    async fn write_file_data(&self, path: &Path, offset: i64, data: &[u8]) -> Result<u32, i32> {
        let nfs_path = self.get_nfs_path(path);
        
        let mut file = match OpenOptions::new()
            .write(true)
            .create(true)
            .open(&nfs_path)
        {
            Ok(f) => f,
            Err(_) => return Err(libc::EACCES),
        };
        
        if let Err(_) = file.seek(SeekFrom::Start(offset as u64)) {
            return Err(libc::EINVAL);
        }
        
        match file.write(data) {
            Ok(bytes_written) => {
                // 写入后需要使缓存无效
                let cache_path = self.get_cache_path(path);
                if cache_path.exists() {
                    let _ = std::fs::remove_file(&cache_path);
                    info!("Invalidated cache for modified file: {}", path.display());
                }
                
                Ok(bytes_written as u32)
            }
            Err(_) => Err(libc::EIO),
        }
    }
    
    /// 列出目录内容
    async fn list_directory(&self, path: &Path) -> Result<Vec<(String, InternalFileAttr)>, i32> {
        let nfs_path = self.get_nfs_path(path);
        
        let entries = match std::fs::read_dir(&nfs_path) {
            Ok(entries) => entries,
            Err(_) => return Err(ENOENT),
        };
        
        let mut result = Vec::new();
        
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            
            let name = entry.file_name().to_string_lossy().to_string();
            let entry_path = path.join(&name);
            
            if let Ok(attr) = self.get_nfs_attr(&entry_path).await {
                result.push((name, attr));
            }
        }
        
        Ok(result)
    }
}

impl Filesystem for CacheFs {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let inode_manager = Arc::clone(&self.inode_manager);
        let config = self.config.clone();
        let name = name.to_os_string();
        
        tokio::spawn(async move {
            let parent_path = if parent == FUSE_ROOT_ID {
                PathBuf::from("/")
            } else {
                match inode_manager.get_path(parent) {
                    Some(path) => path,
                    None => {
                        reply.error(ENOENT);
                        return;
                    }
                }
            };
            
            let file_path = parent_path.join(&name);
            
            // 从 NFS 后端获取属性
            let nfs_path = config.nfs_backend_path.join(file_path.strip_prefix("/").unwrap_or(&file_path));
            
            match std::fs::metadata(&nfs_path) {
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
                        perm: 0o644,
                        nlink: 1,
                        uid: 1000,
                        gid: 1000,
                        rdev: 0,
                        flags: 0,
                    };
                    
                    inode_manager.insert_mapping(file_path, inode, attr.clone());
                    let fuse_attr: FileAttr = attr.into();
                    reply.entry(&Duration::from_secs(1), &fuse_attr, 0);
                }
                Err(_) => reply.error(ENOENT),
            }
        });
    }
    
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let inode_manager = Arc::clone(&self.inode_manager);
        
        tokio::spawn(async move {
            if let Some(attr) = inode_manager.get_attr(ino) {
                let fuse_attr: FileAttr = attr.into();
                reply.attr(&Duration::from_secs(1), &fuse_attr);
            } else {
                reply.error(ENOENT);
            }
        });
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
        let inode_manager = Arc::clone(&self.inode_manager);
        let config = self.config.clone();
        let _cache_manager = Arc::clone(&self.cache_manager);
        
        tokio::spawn(async move {
            let path = match inode_manager.get_path(ino) {
                Some(path) => path,
                None => {
                    reply.error(ENOENT);
                    return;
                }
            };
            
            // 简化的读取实现
            let nfs_path = config.nfs_backend_path.join(path.strip_prefix("/").unwrap_or(&path));
            
            match std::fs::File::open(&nfs_path) {
                Ok(mut file) => {
                    use std::io::{Read, Seek, SeekFrom};
                    if let Err(_) = file.seek(SeekFrom::Start(offset as u64)) {
                        reply.error(libc::EINVAL);
                        return;
                    }
                    
                    let mut buffer = vec![0; size as usize];
                    match file.read(&mut buffer) {
                        Ok(bytes_read) => {
                            buffer.truncate(bytes_read);
                            reply.data(&buffer);
                        }
                        Err(_) => reply.error(libc::EIO),
                    }
                }
                Err(_) => reply.error(ENOENT),
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
        let inode_manager = Arc::clone(&self.inode_manager);
        let config = self.config.clone();
        let data = data.to_vec();
        
        tokio::spawn(async move {
            let path = match inode_manager.get_path(ino) {
                Some(path) => path,
                None => {
                    reply.error(ENOENT);
                    return;
                }
            };
            
            // 简化的写入实现
            let nfs_path = config.nfs_backend_path.join(path.strip_prefix("/").unwrap_or(&path));
            
            match std::fs::OpenOptions::new().write(true).create(true).open(&nfs_path) {
                Ok(mut file) => {
                    use std::io::{Write, Seek, SeekFrom};
                    if let Err(_) = file.seek(SeekFrom::Start(offset as u64)) {
                        reply.error(libc::EINVAL);
                        return;
                    }
                    
                    match file.write(&data) {
                        Ok(bytes_written) => reply.written(bytes_written as u32),
                        Err(_) => reply.error(libc::EIO),
                    }
                }
                Err(_) => reply.error(libc::EACCES),
            }
        });
    }
    
    fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        let inode_manager = Arc::clone(&self.inode_manager);
        let config = self.config.clone();
        let open_files = Arc::clone(&self.open_files);
        let next_fh = Arc::clone(&self.next_fh);
        
        tokio::spawn(async move {
            let path = match inode_manager.get_path(ino) {
                Some(path) => path,
                None => {
                    reply.error(ENOENT);
                    return;
                }
            };
            
            // 简化实现：直接使用 NFS 路径
            let nfs_path = config.nfs_backend_path.join(path.strip_prefix("/").unwrap_or(&path));
            
            match File::open(&nfs_path) {
                Ok(file) => {
                    let mut next = next_fh.write().await;
                    let fh = *next;
                    *next += 1;
                    
                    open_files.write().await.insert(fh, Arc::new(RwLock::new(file)));
                    reply.opened(fh, 0);
                }
                Err(_) => reply.error(ENOENT),
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
        let inode_manager = Arc::clone(&self.inode_manager);
        let config = self.config.clone();
        
        tokio::spawn(async move {
            let path = if ino == FUSE_ROOT_ID {
                PathBuf::from("/")
            } else {
                match inode_manager.get_path(ino) {
                    Some(path) => path,
                    None => {
                        reply.error(ENOENT);
                        return;
                    }
                }
            };
            
            // 简化的目录读取实现
            let nfs_path = config.nfs_backend_path.join(path.strip_prefix("/").unwrap_or(&path));
            
            match std::fs::read_dir(&nfs_path) {
                Ok(entries) => {
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
                    for entry in entries {
                        if let Ok(entry) = entry {
                            if offset <= index {
                                let name = entry.file_name().to_string_lossy().to_string();
                                let file_type = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                                    FileType::Directory
                                } else {
                                    FileType::RegularFile
                                };
                                
                                // 分配临时 inode
                                let temp_ino = inode_manager.allocate_inode();
                                
                                if reply.add(temp_ino, index + 1, file_type, &name) {
                                    break;
                                }
                            }
                            index += 1;
                        }
                    }
                    
                    reply.ok();
                }
                Err(_) => reply.error(ENOENT),
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
        let open_files = Arc::clone(&self.open_files);
        
        tokio::spawn(async move {
            open_files.write().await.remove(&fh);
            reply.ok();
        });
    }
} 