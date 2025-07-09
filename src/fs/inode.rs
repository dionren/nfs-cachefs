use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::RwLock;
use std::time::SystemTime;

/// inode 编号类型
pub type Inode = u64;

/// 文件属性信息
#[derive(Debug, Clone)]
pub struct FileAttr {
    pub inode: Inode,
    pub size: u64,
    pub blocks: u64,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub crtime: SystemTime,
    pub kind: FileType,
    pub perm: u16,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub flags: u32,
}

/// 文件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Directory,
    RegularFile,
    Symlink,
    NamedPipe,
    BlockDevice,
    CharDevice,
    Socket,
}

impl From<FileType> for fuser::FileType {
    fn from(ft: FileType) -> Self {
        match ft {
            FileType::Directory => fuser::FileType::Directory,
            FileType::RegularFile => fuser::FileType::RegularFile,
            FileType::Symlink => fuser::FileType::Symlink,
            FileType::NamedPipe => fuser::FileType::NamedPipe,
            FileType::BlockDevice => fuser::FileType::BlockDevice,
            FileType::CharDevice => fuser::FileType::CharDevice,
            FileType::Socket => fuser::FileType::Socket,
        }
    }
}

impl From<fuser::FileType> for FileType {
    fn from(ft: fuser::FileType) -> Self {
        match ft {
            fuser::FileType::Directory => FileType::Directory,
            fuser::FileType::RegularFile => FileType::RegularFile,
            fuser::FileType::Symlink => FileType::Symlink,
            fuser::FileType::NamedPipe => FileType::NamedPipe,
            fuser::FileType::BlockDevice => FileType::BlockDevice,
            fuser::FileType::CharDevice => FileType::CharDevice,
            fuser::FileType::Socket => FileType::Socket,
        }
    }
}

impl From<FileAttr> for fuser::FileAttr {
    fn from(attr: FileAttr) -> Self {
        fuser::FileAttr {
            ino: attr.inode,
            size: attr.size,
            blocks: attr.blocks,
            atime: attr.atime,
            mtime: attr.mtime,
            ctime: attr.ctime,
            crtime: attr.crtime,
            kind: attr.kind.into(),
            perm: attr.perm,
            nlink: attr.nlink,
            uid: attr.uid,
            gid: attr.gid,
            rdev: attr.rdev,
            flags: attr.flags,
            blksize: 4096, // 默认块大小
        }
    }
}

/// inode 管理器
pub struct InodeManager {
    /// 路径到 inode 的映射
    path_to_inode: Arc<RwLock<HashMap<PathBuf, Inode>>>,
    /// inode 到路径的映射
    inode_to_path: Arc<RwLock<HashMap<Inode, PathBuf>>>,
    /// inode 到文件属性的映射
    inode_to_attr: Arc<RwLock<HashMap<Inode, FileAttr>>>,
    /// 下一个可用的 inode
    next_inode: Arc<RwLock<Inode>>,
    /// 根目录的 inode（固定为 1）
    root_inode: Inode,
}

impl InodeManager {
    /// 创建新的 inode 管理器
    pub fn new() -> Self {
        let root_inode = 1;
        let manager = Self {
            path_to_inode: Arc::new(RwLock::new(HashMap::new())),
            inode_to_path: Arc::new(RwLock::new(HashMap::new())),
            inode_to_attr: Arc::new(RwLock::new(HashMap::new())),
            next_inode: Arc::new(RwLock::new(2)), // 从 2 开始，1 保留给根目录
            root_inode,
        };
        
        // 初始化根目录
        let root_path = PathBuf::from("/");
        let now = SystemTime::now();
        let root_attr = FileAttr {
            inode: root_inode,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: 0,
            gid: 0,
            rdev: 0,
            flags: 0,
        };
        
        manager.path_to_inode.write().insert(root_path.clone(), root_inode);
        manager.inode_to_path.write().insert(root_inode, root_path);
        manager.inode_to_attr.write().insert(root_inode, root_attr);
        
        manager
    }
    
    /// 获取根目录的 inode
    pub fn root_inode(&self) -> Inode {
        self.root_inode
    }
    
    /// 分配新的 inode
    pub fn allocate_inode(&self) -> Inode {
        let mut next = self.next_inode.write();
        let inode = *next;
        *next += 1;
        inode
    }
    
    /// 根据路径获取 inode
    pub fn get_inode(&self, path: &Path) -> Option<Inode> {
        self.path_to_inode.read().get(path).copied()
    }
    
    /// 根据 inode 获取路径
    pub fn get_path(&self, inode: Inode) -> Option<PathBuf> {
        self.inode_to_path.read().get(&inode).cloned()
    }
    
    /// 根据 inode 获取文件属性
    pub fn get_attr(&self, inode: Inode) -> Option<FileAttr> {
        self.inode_to_attr.read().get(&inode).cloned()
    }
    
    /// 插入新的路径-inode 映射
    pub fn insert_mapping(&self, path: PathBuf, inode: Inode, attr: FileAttr) {
        self.path_to_inode.write().insert(path.clone(), inode);
        self.inode_to_path.write().insert(inode, path);
        self.inode_to_attr.write().insert(inode, attr);
    }
    
    /// 更新文件属性
    pub fn update_attr(&self, inode: Inode, attr: FileAttr) {
        self.inode_to_attr.write().insert(inode, attr);
    }
    
    /// 删除路径-inode 映射
    pub fn remove_mapping(&self, path: &Path) -> Option<Inode> {
        if let Some(inode) = self.path_to_inode.write().remove(path) {
            self.inode_to_path.write().remove(&inode);
            self.inode_to_attr.write().remove(&inode);
            Some(inode)
        } else {
            None
        }
    }
    
    /// 删除 inode 映射
    pub fn remove_inode(&self, inode: Inode) -> Option<PathBuf> {
        if let Some(path) = self.inode_to_path.write().remove(&inode) {
            self.path_to_inode.write().remove(&path);
            self.inode_to_attr.write().remove(&inode);
            Some(path)
        } else {
            None
        }
    }
    
    /// 重命名文件/目录
    pub fn rename(&self, old_path: &Path, new_path: &Path) -> Result<(), String> {
        let mut path_to_inode = self.path_to_inode.write();
        let mut inode_to_path = self.inode_to_path.write();
        
        if let Some(inode) = path_to_inode.remove(old_path) {
            path_to_inode.insert(new_path.to_path_buf(), inode);
            inode_to_path.insert(inode, new_path.to_path_buf());
            Ok(())
        } else {
            Err(format!("Path not found: {}", old_path.display()))
        }
    }
    
    /// 获取所有已知的路径
    pub fn get_all_paths(&self) -> Vec<PathBuf> {
        self.path_to_inode.read().keys().cloned().collect()
    }
    
    /// 获取所有已知的 inode
    pub fn get_all_inodes(&self) -> Vec<Inode> {
        self.inode_to_path.read().keys().copied().collect()
    }
    
    /// 清理缓存（用于测试）
    pub fn clear(&self) {
        self.path_to_inode.write().clear();
        self.inode_to_path.write().clear();
        self.inode_to_attr.write().clear();
        *self.next_inode.write() = 2;
        
        // 重新初始化根目录
        let root_path = PathBuf::from("/");
        let now = SystemTime::now();
        let root_attr = FileAttr {
            inode: self.root_inode,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: 0,
            gid: 0,
            rdev: 0,
            flags: 0,
        };
        
        self.path_to_inode.write().insert(root_path.clone(), self.root_inode);
        self.inode_to_path.write().insert(self.root_inode, root_path);
        self.inode_to_attr.write().insert(self.root_inode, root_attr);
    }
}

impl Default for InodeManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_inode_manager_basic() {
        let manager = InodeManager::new();
        
        // 测试根目录
        assert_eq!(manager.root_inode(), 1);
        assert_eq!(manager.get_inode(&PathBuf::from("/")), Some(1));
        assert_eq!(manager.get_path(1), Some(PathBuf::from("/")));
        
        // 测试属性获取
        let root_attr = manager.get_attr(1).unwrap();
        assert_eq!(root_attr.inode, 1);
        assert_eq!(root_attr.kind, FileType::Directory);
    }
    
    #[test]
    fn test_inode_allocation() {
        let manager = InodeManager::new();
        
        let inode1 = manager.allocate_inode();
        let inode2 = manager.allocate_inode();
        
        assert_eq!(inode1, 2);
        assert_eq!(inode2, 3);
        assert!(inode1 != inode2);
    }
    
    #[test]
    fn test_mapping_operations() {
        let manager = InodeManager::new();
        
        let path = PathBuf::from("/test/file.txt");
        let inode = manager.allocate_inode();
        let now = SystemTime::now();
        
        let attr = FileAttr {
            inode,
            size: 1024,
            blocks: 2,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::RegularFile,
            perm: 0o644,
            nlink: 1,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            flags: 0,
        };
        
        // 插入映射
        manager.insert_mapping(path.clone(), inode, attr.clone());
        
        // 验证映射
        assert_eq!(manager.get_inode(&path), Some(inode));
        assert_eq!(manager.get_path(inode), Some(path.clone()));
        assert_eq!(manager.get_attr(inode).unwrap().size, 1024);
        
        // 删除映射
        let removed_inode = manager.remove_mapping(&path);
        assert_eq!(removed_inode, Some(inode));
        assert_eq!(manager.get_inode(&path), None);
        assert_eq!(manager.get_path(inode), None);
    }
    
    #[test]
    fn test_rename_operation() {
        let manager = InodeManager::new();
        
        let old_path = PathBuf::from("/old/file.txt");
        let new_path = PathBuf::from("/new/file.txt");
        let inode = manager.allocate_inode();
        let now = SystemTime::now();
        
        let attr = FileAttr {
            inode,
            size: 1024,
            blocks: 2,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::RegularFile,
            perm: 0o644,
            nlink: 1,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            flags: 0,
        };
        
        manager.insert_mapping(old_path.clone(), inode, attr);
        
        // 执行重命名
        manager.rename(&old_path, &new_path).unwrap();
        
        // 验证重命名结果
        assert_eq!(manager.get_inode(&old_path), None);
        assert_eq!(manager.get_inode(&new_path), Some(inode));
        assert_eq!(manager.get_path(inode), Some(new_path));
    }
} 