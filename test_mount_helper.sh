#!/bin/bash

# 测试mount helper模式的参数解析
echo "Testing mount helper mode parameter parsing..."

# 创建测试目录
sudo mkdir -p /mnt/test-nfs /mnt/test-cache /mnt/test-mount

# 创建测试文件
echo "test content" | sudo tee /mnt/test-nfs/test.txt > /dev/null

# 测试mount helper模式
echo "Testing with mount helper mode..."
sudo ./target/release/nfs-cachefs cachefs /mnt/test-mount -o nfs_backend=/mnt/test-nfs,cache_dir=/mnt/test-cache,cache_size_gb=10,allow_other

echo "Test completed."