#!/bin/bash

# NFS-CacheFS v0.1.0 安装脚本
# 适用于 Ubuntu 22.04/24.04

set -e

echo "=== NFS-CacheFS v0.1.0 安装脚本 ==="
echo "适用于 Ubuntu 22.04/24.04"
echo

# 检查是否为root用户
if [ "$EUID" -ne 0 ]; then
    echo "错误: 请使用 sudo 运行此脚本"
    exit 1
fi

# 检查系统版本
if ! command -v lsb_release &> /dev/null; then
    echo "警告: 无法检测系统版本，假设为兼容的Ubuntu版本"
else
    VERSION=$(lsb_release -rs)
    if [[ "$VERSION" != "22.04" && "$VERSION" != "24.04" ]]; then
        echo "警告: 此脚本专为Ubuntu 22.04/24.04设计，当前版本: $VERSION"
        echo "继续安装可能出现兼容性问题"
        read -p "是否继续? (y/N): " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            exit 1
        fi
    fi
fi

echo "步骤 1/4: 更新包管理器..."
apt update

echo "步骤 2/4: 安装必要依赖..."
apt install -y libfuse3-3 fuse3

echo "步骤 3/4: 安装 nfs-cachefs 二进制文件..."
# 复制二进制文件到系统路径
cp nfs-cachefs /usr/local/bin/
chmod +x /usr/local/bin/nfs-cachefs

# 创建 mount helper 符号链接
ln -sf /usr/local/bin/nfs-cachefs /sbin/mount.cachefs

echo "步骤 4/4: 创建必要的目录..."
# 创建默认挂载点和缓存目录
mkdir -p /mnt/cached
mkdir -p /mnt/cache

echo
echo "✅ 安装完成！"
echo
echo "基本使用方法："
echo "  sudo nfs-cachefs --help"
echo "  sudo nfs-cachefs --version"
echo
echo "手动挂载示例："
echo "  sudo mount -t cachefs cachefs /mnt/cached \\"
echo "    -o nfs_backend=/mnt/nfs-share,cache_dir=/mnt/cache,cache_size_gb=50,allow_other"
echo
echo "更多详细信息请参考 INSTALL.md"