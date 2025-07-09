#!/bin/bash

# 下载 nfs-cachefs 发布版本
echo "正在下载 nfs-cachefs v0.1..."

# 正确的 GitHub raw 链接
RELEASE_URL="https://raw.githubusercontent.com/dionren/nfs-cachefs/main/nfs-cachefs-v0.1-linux-x86_64.tar.gz"
FILENAME="nfs-cachefs-v0.1-linux-x86_64.tar.gz"

# 下载文件
echo "从 $RELEASE_URL 下载..."
wget -O "$FILENAME" "$RELEASE_URL"

# 检查下载是否成功
if [ $? -eq 0 ]; then
    echo "下载成功！"
    
    # 验证文件格式
    echo "验证文件格式..."
    if tar -tzf "$FILENAME" > /dev/null 2>&1; then
        echo "文件格式正确，可以解压"
        echo "解压文件..."
        tar -xzf "$FILENAME"
        echo "解压完成！"
        echo "文件内容："
        ls -la
    else
        echo "错误：文件不是有效的 tar.gz 格式"
        echo "文件内容："
        head -5 "$FILENAME"
    fi
else
    echo "下载失败！"
fi