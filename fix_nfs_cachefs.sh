#!/bin/bash

# NFS-CacheFS 挂载修复脚本

echo "正在修复 NFS-CacheFS 挂载问题..."

# 1. 杀死可能卡住的进程
echo "1. 清理进程..."
sudo pkill -9 -f nfs-cachefs 2>/dev/null || true
sudo fusermount3 -u /mnt/chenyu-nfs 2>/dev/null || true
sudo umount /mnt/chenyu-nfs 2>/dev/null || true

# 2. 检查并创建必要的目录
echo "2. 创建必要的目录..."
sudo mkdir -p /mnt/chenyu-nvme /mnt/nvme/cachefs /mnt/chenyu-nfs

# 3. 挂载NFS后端（如果还没有挂载）
echo "3. 检查NFS后端挂载..."
if ! mountpoint -q /mnt/chenyu-nvme; then
    echo "挂载NFS后端..."
    sudo mount /dev/nvme0n1p1 /mnt/chenyu-nvme
fi

# 4. 检查目录内容
echo "4. 验证目录结构..."
ls -la /mnt/chenyu-nvme | head -5
ls -la /mnt/nvme/cachefs
ls -la /mnt/chenyu-nfs

# 5. 安装必要的依赖（如果还没有安装）
echo "5. 检查依赖..."
if ! dpkg -l | grep -q libfuse3-3; then
    echo "安装FUSE3依赖..."
    sudo apt update
    sudo apt install -y libfuse3-3 fuse3
fi

# 6. 使用正确的mount命令格式
echo "6. 挂载 NFS-CacheFS..."
sudo mount -t cachefs cachefs /mnt/chenyu-nfs \
    -o nfs_backend=/mnt/chenyu-nvme,cache_dir=/mnt/nvme/cachefs,cache_size_gb=100,allow_other

# 7. 验证挂载结果
echo "7. 验证挂载结果..."
if mountpoint -q /mnt/chenyu-nfs; then
    echo "✅ NFS-CacheFS 挂载成功！"
    mount | grep chenyu-nfs
    ls -la /mnt/chenyu-nfs | head -5
else
    echo "❌ 挂载失败，尝试手动方式..."
    # 手动方式
    sudo /workspace/nfs-cachefs-v0.2.0-linux-x86_64/nfs-cachefs \
        /mnt/chenyu-nvme /mnt/chenyu-nfs \
        --cache-dir /mnt/nvme/cachefs \
        --cache-size 100 &
    
    sleep 5
    
    if mountpoint -q /mnt/chenyu-nfs; then
        echo "✅ 手动挂载成功！"
    else
        echo "❌ 挂载仍然失败，请检查日志"
    fi
fi

echo "修复脚本执行完成。"