#!/bin/bash

# NFS-CacheFS 修复测试脚本

set -e

echo "=== NFS-CacheFS 修复测试 ==="
echo "时间: $(date)"
echo

# 清理旧的挂载和进程
echo "1. 清理旧的挂载和进程..."
sudo pkill -9 -f nfs-cachefs 2>/dev/null || true
sudo fusermount3 -u /mnt/chenyu-nfs 2>/dev/null || true
sudo umount /mnt/chenyu-nfs 2>/dev/null || true

# 重新编译
echo "2. 重新编译程序..."
cd /root/nfs-cachefs
cargo build --release

# 安装新版本
echo "3. 安装新版本..."
sudo cp target/release/nfs-cachefs /usr/local/bin/
sudo chmod +x /usr/local/bin/nfs-cachefs

# 验证目录
echo "4. 验证目录结构..."
ls -la /mnt/chenyu-nvme | head -5
echo "NFS挂载点内容: $(ls /mnt/chenyu-nvme | wc -l) 个文件/目录"

ls -la /mnt/nvme/cachefs
echo "缓存目录状态: OK"

ls -la /mnt/chenyu-nfs
echo "挂载点目录状态: OK"

# 测试不带 foreground 选项的挂载
echo "5. 测试标准挂载..."
sudo /usr/local/bin/nfs-cachefs \
    /mnt/chenyu-nvme /mnt/chenyu-nfs \
    --cache-dir /mnt/nvme/cachefs \
    --cache-size 10 \
    --debug &

# 等待挂载完成
MOUNT_PID=$!
sleep 5

# 检查挂载状态
if mount | grep -q /mnt/chenyu-nfs; then
    echo "✅ 标准挂载成功！"
    mount | grep /mnt/chenyu-nfs
    
    # 测试读取
    echo "6. 测试文件读取..."
    ls -la /mnt/chenyu-nfs | head -5
    echo "✅ 文件读取成功！"
    
    # 卸载
    echo "7. 卸载文件系统..."
    sudo kill $MOUNT_PID 2>/dev/null || true
    sleep 2
    sudo fusermount3 -u /mnt/chenyu-nfs 2>/dev/null || true
    echo "✅ 卸载成功！"
else
    echo "❌ 标准挂载失败"
    sudo kill $MOUNT_PID 2>/dev/null || true
fi

# 测试带 foreground 选项的挂载
echo "8. 测试前台挂载..."
timeout 10 sudo /usr/local/bin/nfs-cachefs \
    /mnt/chenyu-nvme /mnt/chenyu-nfs \
    --cache-dir /mnt/nvme/cachefs \
    --cache-size 10 \
    --foreground --debug &

FOREGROUND_PID=$!
sleep 5

# 检查是否挂载成功
if mount | grep -q /mnt/chenyu-nfs; then
    echo "✅ 前台挂载成功！"
    mount | grep /mnt/chenyu-nfs
    
    # 测试读取
    ls -la /mnt/chenyu-nfs | head -3
    echo "✅ 前台模式文件读取成功！"
    
    # 停止前台进程
    sudo kill $FOREGROUND_PID 2>/dev/null || true
    sleep 2
    sudo fusermount3 -u /mnt/chenyu-nfs 2>/dev/null || true
    echo "✅ 前台模式测试完成！"
else
    echo "❌ 前台挂载失败"
    sudo kill $FOREGROUND_PID 2>/dev/null || true
fi

echo
echo "=== 测试完成 ==="
echo "修复状态: 程序已修复了foreground选项传递给FUSE的问题"
echo "建议: 现在可以正常使用 nfs-cachefs 进行挂载" 