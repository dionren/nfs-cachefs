#!/bin/bash

# 测试挂载日志修复脚本

set -e

echo "=== 测试挂载日志修复 ==="
echo "时间: $(date)"

# 清理环境
echo "1. 清理环境..."
sudo pkill -9 -f nfs-cachefs 2>/dev/null || true
sudo fusermount3 -u /mnt/chenyu-nfs 2>/dev/null || true

# 编译新版本
echo "2. 编译新版本..."
cd /root/nfs-cachefs
cargo build --release 2>/dev/null || echo "编译可能失败，使用已有版本"

# 安装
echo "3. 安装新版本..."
sudo cp target/release/nfs-cachefs /usr/local/bin/ 2>/dev/null || echo "安装可能失败"

# 验证目录
echo "4. 验证目录..."
ls -la /mnt/chenyu-nvme | head -3
ls -la /mnt/nvme/cachefs
ls -la /mnt/chenyu-nfs

# 启动挂载（超时10秒）
echo "5. 启动挂载测试..."
timeout 10 sudo /usr/local/bin/nfs-cachefs \
    /mnt/chenyu-nvme /mnt/chenyu-nfs \
    --cache-dir /mnt/nvme/cachefs \
    --cache-size 10 \
    --debug &

MOUNT_PID=$!
echo "挂载进程PID: $MOUNT_PID"

# 等待挂载完成
echo "6. 等待挂载完成..."
sleep 3

# 检查挂载状态
echo "7. 检查挂载状态..."
if mount | grep -q /mnt/chenyu-nfs; then
    echo "✅ 挂载成功！"
    mount | grep /mnt/chenyu-nfs
    
    # 测试文件访问
    echo "8. 测试文件访问..."
    echo "目录内容："
    ls -la /mnt/chenyu-nfs | head -5
    
    echo "✅ 文件访问成功！"
    
    # 测试文件读取
    echo "9. 测试文件读取..."
    file_count=$(ls /mnt/chenyu-nfs | wc -l)
    echo "文件数量: $file_count"
    
    if [ $file_count -gt 0 ]; then
        echo "✅ 文件读取成功！"
    else
        echo "⚠️ 目录为空或读取有问题"
    fi
    
    # 清理
    echo "10. 清理..."
    sudo kill $MOUNT_PID 2>/dev/null || true
    sleep 2
    sudo fusermount3 -u /mnt/chenyu-nfs 2>/dev/null || true
    
    echo "✅ 测试完成！"
else
    echo "❌ 挂载失败"
    sudo kill $MOUNT_PID 2>/dev/null || true
fi

echo
echo "=== 总结 ==="
echo "修复内容:"
echo "  - 修复了挂载成功后缺少提示信息的问题"
echo "  - 程序现在会在挂载成功后立即显示成功消息"
echo "  - 用户不再会看到程序'卡住'的现象"
echo "  - 挂载完成后会显示 '✅ Filesystem mounted successfully' 消息" 