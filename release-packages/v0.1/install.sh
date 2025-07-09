#!/bin/bash

set -e

echo "Installing NFS-CacheFS v0.1..."

# 检查是否为root用户
if [[ $EUID -ne 0 ]]; then
   echo "This script must be run as root (use sudo)" 
   exit 1
fi

# 复制二进制文件
cp nfs-cachefs /usr/local/bin/
chmod +x /usr/local/bin/nfs-cachefs

# 创建符号链接以支持mount命令
ln -sf /usr/local/bin/nfs-cachefs /sbin/mount.cachefs

echo "Installation completed successfully!"
echo "Run 'nfs-cachefs --version' to verify installation."