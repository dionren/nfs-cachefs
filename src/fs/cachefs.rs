use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt, FileExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use dashmap::DashMap;
use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, ReplyStatfs, Request, FUSE_ROOT_ID,
};
use tracing::{error, info, warn, debug};

use crate::cache::manager::CacheManager;
use crate::cache::state::CacheStatus;
use crate::core::config::Config;
use crate::fs::inode::{InodeManager, FileAttr as InternalFileAttr, FileType as InternalFileType};

/// FUSE 属性缓存超时
const TTL: Duration = Duration::from_secs(10);

/// FOPEN_KEEP_CACHE: 告诉内核在 close/open 之间保持文件的页面缓存。
/// 对于已缓存且 mtime 未变的文件，后续读取可直接由内核页面缓存服务，
/// 完全绕过 FUSE read 回调，达到接近直接读取本地文件的速度。
const FOPEN_KEEP_CACHE: u32 = 1 << 1;

/// 打开文件的读取来源
enum OpenFileSource {
    /// 从本地缓存读取（快速路径）
    Cache(std::fs::File),
    /// 从 NFS 后端读取
    Nfs(std::fs::File),
}

/// 已打开文件的上下文信息
struct OpenFileHandle {
    /// 预打开的文件描述符（在 open 时决定来源，read 时直接使用）
    source: OpenFileSource,
    /// 本地缓存路径（用于 record_access）
    cache_path: PathBuf,
}

/// NFS-CacheFS 只读文件系统实现
///
/// 性能关键设计：
/// - open() 时做一次 NFS metadata 调用验证缓存有效性，预打开文件 fd
/// - read() 时通过预打开的 fd 使用 pread 读取，零 NFS 元数据开销
/// - FOPEN_KEEP_CACHE 使内核缓存页面数据，重复读取不经过 FUSE
/// - release() 清理 fd
#[derive(Clone)]
pub struct CacheFs {
    /// inode 管理器
    inode_manager: Arc<InodeManager>,
    /// 缓存管理器
    cache_manager: Arc<CacheManager>,
    /// 配置信息
    config: Arc<Config>,
    /// Tokio 运行时句柄（仅用于后台缓存任务和异步 record_access）
    runtime_handle: tokio::runtime::Handle,
    /// 下一个可用文件句柄
    next_fh: Arc<AtomicU64>,
    /// 已打开文件的 fd 缓存（按 fh 索引）
    /// open() 预打开文件，read() 通过 pread 直接使用，release() 清理
    open_files: Arc<DashMap<u64, OpenFileHandle>>,
}

impl CacheFs {
    /// 创建新的 CacheFS 实例
    pub fn new(config: Config, runtime_handle: tokio::runtime::Handle) -> std::io::Result<Self> {
        let inode_manager = Arc::new(InodeManager::new());
        let metrics = Arc::new(crate::cache::metrics::MetricsCollector::new());
        let cache_manager = Arc::new(CacheManager::new(Arc::new(config.clone()), metrics)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?);

        // 用 NFS 后端根目录的真实属性更新根 inode
        if let Ok(metadata) = std::fs::symlink_metadata(&config.nfs_backend_path) {
            let root_attr = Self::build_attr(FUSE_ROOT_ID, &metadata);
            inode_manager.update_attr(FUSE_ROOT_ID, root_attr);
        }

        Ok(Self {
            inode_manager,
            cache_manager,
            config: Arc::new(config),
            runtime_handle,
            next_fh: Arc::new(AtomicU64::new(1)),
            open_files: Arc::new(DashMap::new()),
        })
    }

    /// 优雅关闭文件系统
    pub async fn shutdown(&self) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!("Shutting down CacheFS...");
        if let Err(e) = self.cache_manager.shutdown().await {
            tracing::warn!("Error shutting down cache manager: {}", e);
        }
        tracing::info!("CacheFS shutdown completed");
        Ok(())
    }

    /// 获取缓存路径
    fn get_cache_path(&self, path: &PathBuf) -> PathBuf {
        self.config.cache_dir.join(path.strip_prefix("/").unwrap_or(path))
    }

    /// 获取 NFS 路径
    fn get_nfs_path(&self, path: &PathBuf) -> PathBuf {
        self.config.nfs_backend_path.join(path.strip_prefix("/").unwrap_or(path))
    }

    /// 从 std::fs::Metadata 构建 InternalFileAttr
    fn build_attr(inode: u64, metadata: &std::fs::Metadata) -> InternalFileAttr {
        let file_type = if metadata.is_dir() {
            InternalFileType::Directory
        } else if metadata.file_type().is_symlink() {
            InternalFileType::Symlink
        } else {
            InternalFileType::RegularFile
        };

        InternalFileAttr {
            inode,
            size: metadata.len(),
            blocks: metadata.blocks(),
            atime: metadata.accessed().unwrap_or(SystemTime::now()),
            mtime: metadata.modified().unwrap_or(SystemTime::now()),
            ctime: metadata.modified().unwrap_or(SystemTime::now()),
            crtime: metadata.created().unwrap_or(SystemTime::now()),
            kind: file_type,
            perm: (metadata.mode() & 0o7777) as u16,
            nlink: metadata.nlink() as u32,
            uid: metadata.uid(),
            gid: metadata.gid(),
            rdev: metadata.rdev() as u32,
            flags: 0,
        }
    }

    /// 使用 pread 从已打开的文件读取（线程安全，无需 seek，多线程可并发调用）
    fn pread_file(file: &std::fs::File, offset: i64, size: u32) -> Result<Vec<u8>, i32> {
        let mut buffer = vec![0u8; size as usize];
        let mut total_read = 0;

        while total_read < size as usize {
            match file.read_at(&mut buffer[total_read..], offset as u64 + total_read as u64) {
                Ok(0) => break, // EOF
                Ok(n) => total_read += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(libc::EIO),
            }
        }

        buffer.truncate(total_read);
        Ok(buffer)
    }

    /// 回退用：从路径打开文件并读取（每次 open+seek+read+close）
    fn read_file(file_path: &PathBuf, offset: i64, size: u32) -> Result<Vec<u8>, i32> {
        use std::io::{Read, Seek, SeekFrom};
        use std::fs::File;

        let mut file = match File::open(file_path) {
            Ok(f) => f,
            Err(e) => {
                debug!("Failed to open {}: {}", file_path.display(), e);
                return Err(libc::ENOENT);
            }
        };

        if let Err(_) = file.seek(SeekFrom::Start(offset as u64)) {
            return Err(libc::EINVAL);
        }

        let mut buffer = vec![0u8; size as usize];
        let mut total_read = 0;

        while total_read < size as usize {
            match file.read(&mut buffer[total_read..]) {
                Ok(0) => break,
                Ok(n) => total_read += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(libc::EIO),
            }
        }

        buffer.truncate(total_read);
        Ok(buffer)
    }

    /// 验证缓存 mtime 是否与 NFS 源文件一致
    /// 使用已获取的 NFS metadata，不产生额外的 NFS 网络调用
    fn validate_cache_mtime(&self, cache_path: &PathBuf, nfs_metadata: &std::fs::Metadata) -> bool {
        if let Some(entry) = self.cache_manager.get_entry(cache_path) {
            if let CacheStatus::Cached { source_mtime: Some(cached_mtime), .. } = &entry.status {
                if let Ok(current_mtime) = nfs_metadata.modified() {
                    return *cached_mtime == current_mtime;
                }
            }
        }
        // 无 mtime 信息时假定有效
        true
    }

    /// 触发后台缓存任务（非阻塞）
    fn trigger_background_cache(&self, nfs_path: &PathBuf, file_size: u64) {
        if file_size < self.config.min_cache_file_size {
            return;
        }

        let cache_manager = Arc::clone(&self.cache_manager);
        let nfs_path = nfs_path.clone();

        self.runtime_handle.spawn(async move {
            // 延迟执行，让前台读取优先
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            if let Err(e) = cache_manager.submit_cache_task(
                nfs_path,
                crate::cache::state::CachePriority::Low,
            ).await {
                debug!("Background cache task failed: {}", e);
            }
        });
    }
}

impl Filesystem for CacheFs {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let parent_path = if parent == FUSE_ROOT_ID {
            PathBuf::from("/")
        } else {
            match self.inode_manager.get_path(parent) {
                Some(path) => path,
                None => { reply.error(libc::ENOENT); return; }
            }
        };

        let child_path = parent_path.join(name.to_string_lossy().as_ref());
        let nfs_path = self.get_nfs_path(&child_path);

        // 使用 symlink_metadata 正确处理符号链接
        match std::fs::symlink_metadata(&nfs_path) {
            Ok(metadata) => {
                let inode = self.inode_manager.get_or_allocate_inode(&child_path);
                let attr = Self::build_attr(inode, &metadata);
                self.inode_manager.insert_mapping(child_path, inode, attr.clone());
                let fuse_attr: FileAttr = attr.into();
                reply.entry(&TTL, &fuse_attr, 0);
            }
            Err(_) => reply.error(libc::ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        // 优先使用缓存的属性
        if let Some(attr) = self.inode_manager.get_attr(ino) {
            let fuse_attr: FileAttr = attr.into();
            reply.attr(&TTL, &fuse_attr);
            return;
        }

        // 回退：从 NFS 后端重新获取属性
        if let Some(path) = self.inode_manager.get_path(ino) {
            let nfs_path = self.get_nfs_path(&path);
            match std::fs::symlink_metadata(&nfs_path) {
                Ok(metadata) => {
                    let attr = Self::build_attr(ino, &metadata);
                    self.inode_manager.update_attr(ino, attr.clone());
                    let fuse_attr: FileAttr = attr.into();
                    reply.attr(&TTL, &fuse_attr);
                }
                Err(_) => reply.error(libc::ENOENT),
            }
        } else {
            reply.error(libc::ENOENT);
        }
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
        // 快速路径：通过 open() 预打开的 fd + pread 读取
        // 无 NFS 元数据调用，无文件 open/close 开销
        if let Some(handle) = self.open_files.get(&fh) {
            let (file, is_cache) = match &handle.source {
                OpenFileSource::Cache(f) => (f, true),
                OpenFileSource::Nfs(f) => (f, false),
            };

            match Self::pread_file(file, offset, size) {
                Ok(data) => {
                    if is_cache {
                        // 异步记录访问，避免在读取热路径上的 eviction mutex 竞争
                        let cache_manager = Arc::clone(&self.cache_manager);
                        let cache_path = handle.cache_path.clone();
                        self.runtime_handle.spawn(async move {
                            cache_manager.record_access(&cache_path);
                        });
                    }
                    reply.data(&data);
                    return;
                }
                Err(e) => {
                    warn!("pread failed for fh={}: errno {}", fh, e);
                }
            }
        }

        // 回退路径：无 fd 缓存或 pread 失败
        let path = match self.inode_manager.get_path(ino) {
            Some(path) => path,
            None => { reply.error(libc::ENOENT); return; }
        };

        let nfs_path = self.get_nfs_path(&path);
        self.cache_manager.record_miss();

        match Self::read_file(&nfs_path, offset, size) {
            Ok(data) => {
                reply.data(&data);
            }
            Err(err) => {
                error!("NFS read failed for {}: errno {}", nfs_path.display(), err);
                reply.error(err);
            }
        }
    }

    fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        let path = match self.inode_manager.get_path(ino) {
            Some(path) => path,
            None => { reply.error(libc::ENOENT); return; }
        };

        let nfs_path = self.get_nfs_path(&path);
        let cache_path = self.get_cache_path(&path);

        // 验证文件存在并刷新属性 — open() 时的唯一 NFS 元数据调用
        let nfs_metadata = match std::fs::metadata(&nfs_path) {
            Ok(m) => m,
            Err(_) => { reply.error(libc::ENOENT); return; }
        };
        let attr = Self::build_attr(ino, &nfs_metadata);
        self.inode_manager.update_attr(ino, attr);

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);

        // 在 open 时决定读取来源并预打开 fd
        // 后续所有 read() 调用直接使用此 fd，不再访问 NFS 元数据
        let mut source = None;
        let mut fuse_flags = 0u32;

        if cache_path.exists() {
            if self.validate_cache_mtime(&cache_path, &nfs_metadata) {
                if let Ok(file) = std::fs::File::open(&cache_path) {
                    source = Some(OpenFileSource::Cache(file));
                    // FOPEN_KEEP_CACHE: 内核保持页面缓存，后续读取可完全绕过 FUSE
                    fuse_flags = FOPEN_KEEP_CACHE;
                }
            } else {
                info!("Cache stale at open for {}, invalidating", path.display());
                self.cache_manager.invalidate(&cache_path);
            }
        }

        if source.is_none() {
            match std::fs::File::open(&nfs_path) {
                Ok(file) => {
                    self.trigger_background_cache(&nfs_path, nfs_metadata.len());
                    source = Some(OpenFileSource::Nfs(file));
                }
                Err(_) => { reply.error(libc::EIO); return; }
            }
        }

        self.open_files.insert(fh, OpenFileHandle {
            source: source.unwrap(),
            cache_path,
        });

        reply.opened(fh, fuse_flags);
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let path = if ino == FUSE_ROOT_ID {
            PathBuf::from("/")
        } else {
            match self.inode_manager.get_path(ino) {
                Some(path) => path,
                None => { reply.error(libc::ENOENT); return; }
            }
        };

        let nfs_path = self.get_nfs_path(&path);

        let entries = match std::fs::read_dir(&nfs_path) {
            Ok(entries) => entries,
            Err(_) => { reply.error(libc::ENOENT); return; }
        };

        let mut index = 0i64;

        // "." 条目
        if offset <= index {
            if reply.add(ino, index + 1, FileType::Directory, ".") {
                reply.ok();
                return;
            }
        }
        index += 1;

        // ".." 条目
        if offset <= index {
            let parent_ino = if ino == FUSE_ROOT_ID {
                FUSE_ROOT_ID
            } else {
                path.parent()
                    .and_then(|p| self.inode_manager.get_inode(p))
                    .unwrap_or(FUSE_ROOT_ID)
            };
            if reply.add(parent_ino, index + 1, FileType::Directory, "..") {
                reply.ok();
                return;
            }
        }
        index += 1;

        // 实际目录条目
        for entry_result in entries {
            let entry = match entry_result {
                Ok(e) => e,
                Err(_) => continue,
            };

            if offset <= index {
                let name = entry.file_name();
                let child_path = path.join(name.to_string_lossy().as_ref());
                let nfs_child_path = self.get_nfs_path(&child_path);

                // 获取或分配一致的 inode
                let child_ino = self.inode_manager.get_or_allocate_inode(&child_path);

                let file_type = match entry.file_type() {
                    Ok(ft) if ft.is_dir() => FileType::Directory,
                    Ok(ft) if ft.is_symlink() => FileType::Symlink,
                    _ => FileType::RegularFile,
                };

                // 创建完整的属性映射
                if let Ok(metadata) = std::fs::symlink_metadata(&nfs_child_path) {
                    let attr = Self::build_attr(child_ino, &metadata);
                    self.inode_manager.insert_mapping(child_path, child_ino, attr);
                }

                if reply.add(child_ino, index + 1, file_type, name) {
                    break;
                }
            }
            index += 1;
        }

        reply.ok();
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
        // 清理预打开的文件描述符
        self.open_files.remove(&fh);
        reply.ok();
    }

    fn readlink(&mut self, _req: &Request, ino: u64, reply: ReplyData) {
        let path = match self.inode_manager.get_path(ino) {
            Some(path) => path,
            None => { reply.error(libc::ENOENT); return; }
        };

        let nfs_path = self.get_nfs_path(&path);

        match std::fs::read_link(&nfs_path) {
            Ok(target) => {
                use std::os::unix::ffi::OsStrExt;
                reply.data(target.as_os_str().as_bytes());
            }
            Err(e) => {
                debug!("readlink failed for {}: {}", nfs_path.display(), e);
                reply.error(libc::ENOENT);
            }
        }
    }

    fn statfs(&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
        // 使用 libc::statvfs 直接获取文件系统统计信息
        use std::ffi::CString;
        let path_cstr = match CString::new(
            self.config.nfs_backend_path.to_string_lossy().as_bytes()
        ) {
            Ok(s) => s,
            Err(_) => { reply.error(libc::EINVAL); return; }
        };

        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statvfs(path_cstr.as_ptr(), &mut stat) };

        if ret == 0 {
            reply.statfs(
                stat.f_blocks,
                stat.f_bfree,
                stat.f_bavail,
                stat.f_files,
                stat.f_ffree,
                stat.f_bsize as u32,
                stat.f_namemax as u32,
                stat.f_frsize as u32,
            );
        } else {
            warn!("statfs failed: errno {}", std::io::Error::last_os_error());
            reply.error(libc::EIO);
        }
    }

    fn access(&mut self, _req: &Request, ino: u64, mask: i32, reply: fuser::ReplyEmpty) {
        // 只读文件系统：拒绝写操作
        if mask & libc::W_OK != 0 {
            reply.error(libc::EROFS);
            return;
        }

        // 检查文件是否存在
        if ino == FUSE_ROOT_ID {
            reply.ok();
            return;
        }

        if let Some(path) = self.inode_manager.get_path(ino) {
            let nfs_path = self.get_nfs_path(&path);
            if nfs_path.exists() {
                reply.ok();
            } else {
                reply.error(libc::ENOENT);
            }
        } else {
            reply.error(libc::ENOENT);
        }
    }
}
