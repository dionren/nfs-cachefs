# NFS-CacheFS

一个高性能的异步只读缓存文件系统，专为加速NFS上大文件访问而设计。

## 特性

- ⚡ **零延迟首次访问** - 异步缓存填充，不阻塞首次读取
- 🚀 **透明加速** - 对应用程序完全透明，无需修改代码
- 💾 **智能缓存管理** - 自动LRU驱逐，高效利用NVMe空间
- 🔒 **数据完整性** - 原子操作确保缓存文件始终完整
- 📊 **实时监控** - 内置性能指标和健康检查
- 🔐 **只读模式** - 专为只读工作负载优化，确保数据安全

## 快速开始

### 依赖要求

- Rust 1.75+
- FUSE 3.0+
- Linux Kernel 5.4+

### 编译安装

```bash
# 克隆项目
git clone https://github.com/your-org/nfs-cachefs.git
cd nfs-cachefs

# 编译发布版本
cargo build --release

# 安装到系统
sudo cp target/release/nfs-cachefs /usr/local/bin/
```

### 基本使用

```bash
# 创建 mount helper 链接（首次安装）
sudo ln -s /usr/local/bin/nfs-cachefs /sbin/mount.cachefs

# 创建挂载点和缓存目录
sudo mkdir -p /mnt/cached /mnt/nvme/cache

# 手动挂载 (自动强制只读模式)
sudo mount -t cachefs cachefs /mnt/cached \
    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/nvme/cache,cache_size_gb=50,allow_other
```

### 通过fstab自动挂载

在 `/etc/fstab` 中添加：

```fstab
# 1. 挂载NFS（必须在CacheFS之前）
10.20.66.201:/share    /mnt/nfs    nfs    defaults,_netdev    0 0

# 2. 挂载本地缓存盘（如果需要）
/dev/nvme0n1    /mnt/nvme    xfs    defaults,noatime    0 0

# 3. 挂载CacheFS（自动只读模式）
cachefs    /mnt/cached    cachefs    nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,cache_size_gb=50,allow_other,_netdev    0 0
```

高级配置示例：
```fstab
# 使用所有优化参数的配置（只读模式）
cachefs    /mnt/cached    cachefs    nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,cache_size_gb=100,block_size_mb=4,max_concurrent=8,direct_io=true,readahead_mb=16,eviction=lru,allow_other,_netdev    0 0
```

## 重要说明

### 只读模式

NFS-CacheFS 现在专门设计为只读文件系统：

- ✅ 支持所有读操作（read, readdir, lookup, getattr）
- ❌ 不支持写操作（write, create, mkdir, unlink 等）
- 🔒 文件权限自动设置为只读
- 📁 目录权限设置为 0o555，文件权限设置为 0o444
- ⚠️ 即使指定 `rw` 选项也会被忽略并警告

### 适用场景

- 深度学习模型文件访问
- 大型数据集的只读访问
- 代码仓库的只读访问
- 静态资源分发
- 备份数据的快速访问

## 项目结构

```
nfs-cachefs/
├── src/
│   ├── main.rs           # 程序入口
│   ├── lib.rs            # 库入口
│   ├── core/             # 核心模块
│   │   ├── config.rs     # 配置管理
│   │   └── error.rs      # 错误处理
│   ├── fs/               # 文件系统实现
│   │   ├── cachefs.rs    # FUSE文件系统
│   │   ├── inode.rs      # inode管理
│   │   └── async_executor.rs # 异步操作执行器
│   ├── cache/            # 缓存管理
│   │   ├── manager.rs    # 缓存管理器
│   │   ├── state.rs      # 缓存状态
│   │   └── metrics.rs    # 性能指标
│   └── utils/            # 工具函数
├── docs/                 # 文档
├── tests/                # 测试
└── benches/             # 性能测试
```

## 架构概览

```mermaid
graph TD
    A[应用程序] --> B[CacheFS FUSE层]
    B --> C{缓存状态?}
    C -->|已缓存| D[NVMe缓存]
    C -->|未缓存| E[NFS后端]
    C -->|缓存中| E
    B --> F[异步缓存管理器]
    F --> G[后台复制任务]
    G --> D
```

## 性能对比

| 场景 | 直接NFS | NFS-CacheFS (首次) | NFS-CacheFS (缓存后) |
|------|---------|-------------------|----------------------|
| 10GB文件顺序读 | 100s | 100s | 10s |
| 随机访问延迟 | 10ms | 10ms | 0.1ms |
| 并发读取吞吐量 | 1GB/s | 1GB/s | 10GB/s |

## 开发

### 运行测试

```bash
# 单元测试
cargo test

# 集成测试
cargo test --test integration

# 性能测试
cargo bench
```

### 调试模式

```bash
RUST_LOG=debug nfs-cachefs --nfs-backend /mnt/nfs ...
```

## 贡献

欢迎提交Issue和Pull Request！请查看[贡献指南](CONTRIBUTING.md)。

## 许可证

本项目采用 MIT 许可证。详见 [LICENSE](LICENSE) 文件。 