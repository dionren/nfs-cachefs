# NFS-CacheFS 挂载问题修复指南

## 问题描述

NFS-CacheFS 在挂载时卡住，显示以下日志后停止响应：
```
2025-07-09T16:27:16.496350Z  INFO ThreadId(98) 73: Mounting /mnt/chenyu-nfs
```

## 根本原因分析

通过分析发现了以下问题：

1. **缺少 FUSE3 依赖** - 系统没有安装 `libfuse3-3` 和 `fuse3` 包
2. **目录不存在** - 指定的挂载点和缓存目录不存在
3. **mount.cachefs 未正确设置** - 系统无法找到 cachefs 文件系统类型的挂载程序
4. **FUSE 选项格式错误** - 使用了错误的 FUSE 选项格式

## 解决方案

### 步骤 1: 安装必要的依赖

```bash
sudo apt update
sudo apt install -y libfuse3-3 fuse3
```

### 步骤 2: 创建必要的目录

```bash
sudo mkdir -p /mnt/chenyu-nvme /mnt/nvme/cachefs /mnt/chenyu-nfs
```

### 步骤 3: 挂载 NFS 后端

```bash
# 挂载 NVMe 磁盘作为 NFS 后端
sudo mount /dev/nvme0n1p1 /mnt/chenyu-nvme
```

### 步骤 4: 设置 mount.cachefs 符号链接

```bash
sudo ln -sf /workspace/nfs-cachefs-v0.2.0-linux-x86_64/nfs-cachefs /sbin/mount.cachefs
```

### 步骤 5: 使用正确的挂载命令

有两种方式可以挂载 NFS-CacheFS：

#### 方式 1: 使用 mount 命令（推荐）

```bash
sudo mount -t cachefs cachefs /mnt/chenyu-nfs \
    -o nfs_backend=/mnt/chenyu-nvme,cache_dir=/mnt/nvme/cachefs,cache_size_gb=100,allow_other
```

#### 方式 2: 直接使用可执行文件

```bash
sudo /workspace/nfs-cachefs-v0.2.0-linux-x86_64/nfs-cachefs \
    /mnt/chenyu-nvme /mnt/chenyu-nfs \
    --cache-dir /mnt/nvme/cachefs \
    --cache-size 100
```

## 验证挂载结果

```bash
# 检查挂载状态
mount | grep chenyu-nfs

# 检查挂载点
mountpoint /mnt/chenyu-nfs

# 查看挂载内容
ls -la /mnt/chenyu-nfs
```

## 常见问题排查

### 1. 如果挂载仍然卡住

```bash
# 强制杀死进程
sudo pkill -9 -f nfs-cachefs

# 卸载挂载点
sudo fusermount3 -u /mnt/chenyu-nfs
sudo umount /mnt/chenyu-nfs
```

### 2. 检查 FUSE 设备权限

```bash
ls -la /dev/fuse
# 应该显示: crw-rw-rw- 1 root root 10, 229 Jul  9 16:40 /dev/fuse
```

### 3. 检查依赖库

```bash
ldd /workspace/nfs-cachefs-v0.2.0-linux-x86_64/nfs-cachefs
```

### 4. 启用调试模式

```bash
sudo /workspace/nfs-cachefs-v0.2.0-linux-x86_64/nfs-cachefs \
    /mnt/chenyu-nvme /mnt/chenyu-nfs \
    --cache-dir /mnt/nvme/cachefs \
    --cache-size 100 \
    --debug
```

## 自动化脚本

为了简化修复过程，可以使用以下脚本：

```bash
#!/bin/bash

# 自动修复 NFS-CacheFS 挂载问题

echo "开始修复 NFS-CacheFS 挂载问题..."

# 1. 清理环境
sudo pkill -9 -f nfs-cachefs 2>/dev/null || true
sudo fusermount3 -u /mnt/chenyu-nfs 2>/dev/null || true
sudo umount /mnt/chenyu-nfs 2>/dev/null || true

# 2. 安装依赖
sudo apt update
sudo apt install -y libfuse3-3 fuse3

# 3. 创建目录
sudo mkdir -p /mnt/chenyu-nvme /mnt/nvme/cachefs /mnt/chenyu-nfs

# 4. 挂载后端
sudo mount /dev/nvme0n1p1 /mnt/chenyu-nvme

# 5. 设置符号链接
sudo ln -sf /workspace/nfs-cachefs-v0.2.0-linux-x86_64/nfs-cachefs /sbin/mount.cachefs

# 6. 挂载 CacheFS
sudo mount -t cachefs cachefs /mnt/chenyu-nfs \
    -o nfs_backend=/mnt/chenyu-nvme,cache_dir=/mnt/nvme/cachefs,cache_size_gb=100,allow_other

# 7. 验证结果
if mountpoint -q /mnt/chenyu-nfs; then
    echo "✅ NFS-CacheFS 挂载成功！"
    mount | grep chenyu-nfs
else
    echo "❌ 挂载失败，请手动检查"
fi
```

## 系统要求

- Ubuntu 22.04/24.04 LTS
- Linux Kernel 5.4+
- FUSE 3.0+
- 足够的磁盘空间用于缓存

## 注意事项

1. **只读模式**: NFS-CacheFS v0.2.0 是只读文件系统
2. **缓存空间**: 确保缓存目录有足够的空间
3. **权限**: 需要 root 权限进行挂载操作
4. **网络**: 如果使用真实的 NFS 后端，确保网络连接正常

## 性能优化建议

1. 使用 SSD/NVMe 作为缓存存储
2. 根据工作负载调整缓存大小
3. 合理设置块大小和并发任务数
4. 监控缓存命中率和性能指标