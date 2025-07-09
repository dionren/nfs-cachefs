# NFS-CacheFS v0.1.0 安装指南

## 系统要求

- **操作系统**: Ubuntu 22.04 LTS / Ubuntu 24.04 LTS
- **架构**: x86_64 (64位)
- **内核**: Linux 5.4+
- **依赖**: FUSE 3.0+

## 快速安装

### 方法一：使用安装脚本（推荐）

```bash
# 解压发布包
tar -xzf nfs-cachefs-v0.1.0-linux-x86_64.tar.gz
cd nfs-cachefs-v0.1.0-linux-x86_64

# 运行安装脚本
sudo ./install.sh
```

### 方法二：手动安装

```bash
# 1. 安装依赖
sudo apt update
sudo apt install -y libfuse3-3 fuse3

# 2. 复制二进制文件
sudo cp nfs-cachefs /usr/local/bin/
sudo chmod +x /usr/local/bin/nfs-cachefs

# 3. 创建 mount helper 链接
sudo ln -sf /usr/local/bin/nfs-cachefs /sbin/mount.cachefs

# 4. 创建必要目录
sudo mkdir -p /mnt/cached /mnt/cache
```

## 验证安装

```bash
# 检查版本
nfs-cachefs --version

# 查看帮助
nfs-cachefs --help

# 检查依赖
ldd /usr/local/bin/nfs-cachefs
```

## 基本使用

### 前置条件

1. **挂载NFS共享**（必须先完成）：
```bash
sudo mkdir -p /mnt/nfs-share
sudo mount -t nfs 192.168.1.100:/share /mnt/nfs-share
```

2. **准备缓存目录**：
```bash
sudo mkdir -p /mnt/cache
# 如果使用专用NVMe设备
sudo mkfs.xfs /dev/nvme0n1
sudo mount /dev/nvme0n1 /mnt/nvme
sudo mkdir -p /mnt/nvme/cache
```

### 手动挂载

```bash
# 基础挂载
sudo mount -t cachefs cachefs /mnt/cached \
    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/cache,cache_size_gb=50,allow_other

# 高性能配置
sudo mount -t cachefs cachefs /mnt/cached \
    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/nvme/cache,cache_size_gb=100,block_size_mb=4,max_concurrent=8,direct_io=true,readahead_mb=16,eviction=lru,allow_other
```

### 自动挂载（/etc/fstab）

在 `/etc/fstab` 中添加：

```fstab
# NFS 后端挂载（必须在CacheFS之前）
192.168.1.100:/share  /mnt/nfs-share  nfs  defaults,_netdev  0 0

# 缓存设备挂载（可选）
/dev/nvme0n1  /mnt/nvme  xfs  defaults,noatime  0 0

# CacheFS 挂载
cachefs  /mnt/cached  cachefs  nfs_backend=/mnt/nfs-share,cache_dir=/mnt/nvme/cache,cache_size_gb=100,allow_other,_netdev  0 0
```

### 卸载

```bash
# 卸载CacheFS
sudo umount /mnt/cached

# 卸载NFS（如果需要）
sudo umount /mnt/nfs-share
```

## 配置参数

| 参数 | 说明 | 默认值 | 示例 |
|------|------|--------|------|
| `nfs_backend` | NFS后端挂载点 | 必需 | `/mnt/nfs-share` |
| `cache_dir` | 缓存目录路径 | 必需 | `/mnt/cache` |
| `cache_size_gb` | 缓存大小(GB) | 10 | `50` |
| `block_size_mb` | 块大小(MB) | 1 | `4` |
| `max_concurrent` | 最大并发任务 | 4 | `8` |
| `direct_io` | 启用直接IO | false | `true` |
| `readahead_mb` | 预读大小(MB) | 8 | `16` |
| `eviction` | 缓存驱逐策略 | lru | `lru` |
| `allow_other` | 允许其他用户访问 | false | `true` |

## 性能优化

### 1. 缓存设备优化

```bash
# 使用高性能NVMe设备
sudo mkfs.xfs -f /dev/nvme0n1
sudo mount -o noatime,nobarrier /dev/nvme0n1 /mnt/nvme

# 调整文件系统参数
echo mq-deadline > /sys/block/nvme0n1/queue/scheduler
echo 0 > /sys/block/nvme0n1/queue/add_random
```

### 2. 系统参数调优

```bash
# 增加文件描述符限制
echo "* soft nofile 65536" >> /etc/security/limits.conf
echo "* hard nofile 65536" >> /etc/security/limits.conf

# 调整虚拟内存参数
echo "vm.dirty_ratio = 5" >> /etc/sysctl.conf
echo "vm.dirty_background_ratio = 2" >> /etc/sysctl.conf
sysctl -p
```

### 3. 推荐配置

```bash
# 深度学习/AI工作负载
sudo mount -t cachefs cachefs /mnt/cached \
    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/nvme/cache,cache_size_gb=200,block_size_mb=8,max_concurrent=16,direct_io=true,readahead_mb=32,allow_other

# 通用文件访问
sudo mount -t cachefs cachefs /mnt/cached \
    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/cache,cache_size_gb=50,block_size_mb=4,max_concurrent=8,readahead_mb=16,allow_other
```

## 故障排除

### 常见问题

1. **权限错误**：
```bash
# 确保用户在fuse组中
sudo usermod -a -G fuse $USER
# 重新登录生效
```

2. **挂载失败**：
```bash
# 检查NFS后端是否正常挂载
df -h /mnt/nfs-share

# 检查缓存目录权限
ls -la /mnt/cache
sudo chown -R root:root /mnt/cache
```

3. **性能问题**：
```bash
# 检查缓存使用情况
du -sh /mnt/cache/*

# 监控系统资源
htop
iostat -x 1
```

### 日志和调试

```bash
# 启用调试模式
RUST_LOG=debug nfs-cachefs --nfs-backend /mnt/nfs-share --cache-dir /mnt/cache --cache-size-gb 50 /mnt/cached

# 查看系统日志
journalctl -u nfs-cachefs
dmesg | grep -i fuse
```

## 卸载

```bash
# 卸载所有挂载点
sudo umount /mnt/cached

# 删除二进制文件
sudo rm /usr/local/bin/nfs-cachefs
sudo rm /sbin/mount.cachefs

# 清理缓存目录（可选）
sudo rm -rf /mnt/cache/*
```

## 支持信息

- **版本**: v0.1.0
- **构建日期**: 2024-07-09
- **架构**: x86_64-unknown-linux-gnu
- **Rust版本**: 1.75+
- **FUSE版本**: 3.0+

如有问题，请查看项目文档或提交Issue。