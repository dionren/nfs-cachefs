# NFS-CacheFS fstab 配置指南

本文档详细说明如何通过 `/etc/fstab` 配置 NFS-CacheFS 文件系统。

## 基本配置格式

在 `/etc/fstab` 中添加如下格式的条目：

```
cachefs /挂载点 cachefs 选项列表 0 0
```

## 最小配置示例

```fstab
# 必须先挂载 NFS 和本地缓存目录
10.20.66.201:/mnt/share    /mnt/nfs        nfs     defaults,_netdev    0 0
/dev/nvme0n1               /mnt/nvme       xfs     defaults            0 0

# 挂载 CacheFS
cachefs    /mnt/cached    cachefs    nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,cache_size_gb=50,allow_other,_netdev    0 0
```

## 完整配置示例

```fstab
# 带有所有高级选项的配置
cachefs    /mnt/cached    cachefs    nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,cache_size_gb=100,block_size_mb=4,max_concurrent=8,eviction=lru,checksum=true,ttl_hours=24,direct_io=true,readahead_mb=8,allow_other,_netdev    0 0
```

## 配置参数详解

### 必需参数

| 参数 | 说明 | 示例 |
|------|------|------|
| `nfs_backend` | NFS 后端挂载路径 | `nfs_backend=/mnt/nfs` |
| `cache_dir` | 本地缓存目录路径 | `cache_dir=/mnt/nvme/cache` |
| `cache_size_gb` | 最大缓存大小（GB） | `cache_size_gb=50` |

### 可选参数

| 参数 | 默认值 | 说明 | 示例 |
|------|--------|------|------|
| `block_size_mb` | 1 | 缓存块大小（MB） | `block_size_mb=4` |
| `max_concurrent` | 4 | 最大并发缓存任务数 | `max_concurrent=8` |
| `eviction` | lru | 缓存驱逐策略（lru/lfu/arc） | `eviction=lru` |
| `checksum` | false | 是否启用文件校验和 | `checksum=true` |
| `ttl_hours` | 无限制 | 缓存过期时间（小时） | `ttl_hours=24` |
| `direct_io` | true | 是否使用直接I/O | `direct_io=false` |
| `readahead_mb` | 4 | 预读取大小（MB） | `readahead_mb=8` |

### FUSE 标准参数

| 参数 | 说明 |
|------|------|
| `allow_other` | 允许其他用户访问挂载点 |
| `_netdev` | 标记为网络文件系统，确保网络就绪后才挂载 |
| `noauto` | 不自动挂载（可选） |
| `user` | 允许普通用户挂载（可选） |

## 挂载依赖管理

使用 systemd 的依赖管理确保正确的挂载顺序：

```fstab
# 使用 x-systemd.requires-mounts-for 确保依赖挂载点已就绪
cachefs /mnt/cached cachefs x-systemd.requires-mounts-for=/mnt/nfs,x-systemd.requires-mounts-for=/mnt/nvme,nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,cache_size_gb=50 0 0
```

## 性能调优建议

### 大文件场景（深度学习模型）

```fstab
cachefs /mnt/cached cachefs nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,cache_size_gb=200,block_size_mb=4,max_concurrent=4,direct_io=true,readahead_mb=16,allow_other,_netdev 0 0
```

关键参数：
- `block_size_mb=4` - 较大的块大小提高大文件传输效率
- `direct_io=true` - 避免双重缓存
- `readahead_mb=16` - 增大预读取以提高顺序读性能

### 小文件场景（代码仓库）

```fstab
cachefs /mnt/cached cachefs nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,cache_size_gb=50,block_size_mb=1,max_concurrent=16,direct_io=false,eviction=lfu,allow_other,_netdev 0 0
```

关键参数：
- `block_size_mb=1` - 较小的块大小减少浪费
- `max_concurrent=16` - 增加并发数处理大量小文件
- `eviction=lfu` - 使用 LFU 策略更适合小文件访问模式

### 混合负载场景

```fstab
cachefs /mnt/cached cachefs nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,cache_size_gb=100,block_size_mb=2,max_concurrent=8,eviction=arc,checksum=true,ttl_hours=72,allow_other,_netdev 0 0
```

关键参数：
- `eviction=arc` - 自适应替换缓存，平衡 LRU 和 LFU
- `checksum=true` - 确保数据完整性
- `ttl_hours=72` - 3天后自动清理过期缓存

## 故障排查

### 查看挂载状态

```bash
# 检查挂载点
mount | grep cachefs

# 查看详细挂载选项
findmnt /mnt/cached -o OPTIONS
```

### 手动挂载测试

```bash
# 使用 mount 命令手动测试配置
sudo mount -t cachefs cachefs /mnt/cached -o nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,cache_size_gb=50
```

### 查看系统日志

```bash
# 查看挂载相关日志
journalctl -u mnt-cached.mount

# 查看 CacheFS 运行日志
journalctl -t cachefs
```

## 卸载

```bash
# 正常卸载
sudo umount /mnt/cached

# 强制卸载（如果有进程占用）
sudo umount -l /mnt/cached
```

## 注意事项

1. **挂载顺序**：确保 NFS 和缓存目录在 CacheFS 之前挂载
2. **权限设置**：缓存目录需要适当的读写权限
3. **空间预留**：缓存目录所在分区应预留一定空间避免写满
4. **网络依赖**：使用 `_netdev` 标记确保网络就绪后才挂载

## 示例场景配置

### 1. GPU 训练节点配置

```fstab
# NFS 服务器存储训练数据和模型
10.20.66.201:/datasets    /mnt/nfs-datasets    nfs    defaults,nconnect=16,_netdev    0 0

# 本地 NVMe 作为缓存
/dev/nvme0n1    /mnt/nvme    xfs    defaults,noatime    0 0

# CacheFS 加速数据访问
cachefs    /datasets    cachefs    nfs_backend=/mnt/nfs-datasets,cache_dir=/mnt/nvme/cachefs-data,cache_size_gb=400,block_size_mb=4,direct_io=true,readahead_mb=32,max_concurrent=4,allow_other,_netdev    0 0
```

### 2. 开发环境配置

```fstab
# 共享代码仓库
10.20.66.202:/repos    /mnt/nfs-repos    nfs    defaults,_netdev    0 0

# 本地 SSD 缓存
/dev/sda2    /var/cache/cachefs    ext4    defaults    0 0

# CacheFS 提供快速代码访问
cachefs    /repos    cachefs    nfs_backend=/mnt/nfs-repos,cache_dir=/var/cache/cachefs/repos,cache_size_gb=20,eviction=lfu,max_concurrent=16,ttl_hours=168,allow_other,_netdev    0 0
```

### 3. 数据分析平台配置

```fstab
# 数据湖存储
10.20.66.203:/datalake    /mnt/nfs-datalake    nfs    defaults,rsize=1048576,wsize=1048576,_netdev    0 0

# 高速缓存池
/dev/nvme0n1p1    /cache/tier1    xfs    defaults,noatime    0 0

# CacheFS 智能缓存
cachefs    /datalake    cachefs    nfs_backend=/mnt/nfs-datalake,cache_dir=/cache/tier1/datalake,cache_size_gb=1000,block_size_mb=8,eviction=arc,checksum=true,readahead_mb=64,max_concurrent=8,allow_other,_netdev    0 0
``` 