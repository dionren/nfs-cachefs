#!/bin/bash

# NFS-CacheFS v0.1.0 发布验证脚本

set -e

RELEASE_FILE="nfs-cachefs-v0.1.0-linux-x86_64.tar.gz"
EXPECTED_SHA256="c4cefc14af181870c68fdbdca44d62df3930a343f5004e3f3db8469113e85223"

echo "=== NFS-CacheFS v0.1.0 发布验证 ==="
echo

# 检查发布包是否存在
if [ ! -f "$RELEASE_FILE" ]; then
    echo "❌ 错误: 发布包 $RELEASE_FILE 不存在"
    exit 1
fi

echo "✅ 发布包文件存在: $RELEASE_FILE"

# 验证SHA256校验和
echo "正在验证SHA256校验和..."
ACTUAL_SHA256=$(sha256sum "$RELEASE_FILE" | cut -d' ' -f1)

if [ "$ACTUAL_SHA256" = "$EXPECTED_SHA256" ]; then
    echo "✅ SHA256校验和验证通过"
else
    echo "❌ SHA256校验和验证失败"
    echo "期望: $EXPECTED_SHA256"
    echo "实际: $ACTUAL_SHA256"
    exit 1
fi

# 检查发布包内容
echo "正在检查发布包内容..."
TEMP_DIR=$(mktemp -d)
tar -xzf "$RELEASE_FILE" -C "$TEMP_DIR"

REQUIRED_FILES=(
    "v0.1.0/nfs-cachefs"
    "v0.1.0/install.sh"
    "v0.1.0/INSTALL.md"
    "v0.1.0/RELEASE_NOTES.md"
    "v0.1.0/VERSION"
    "v0.1.0/SHA256SUMS"
)

for file in "${REQUIRED_FILES[@]}"; do
    if [ -f "$TEMP_DIR/$file" ]; then
        echo "✅ $file"
    else
        echo "❌ 缺少文件: $file"
        rm -rf "$TEMP_DIR"
        exit 1
    fi
done

# 检查二进制文件权限
if [ -x "$TEMP_DIR/v0.1.0/nfs-cachefs" ]; then
    echo "✅ 二进制文件具有执行权限"
else
    echo "❌ 二进制文件缺少执行权限"
    rm -rf "$TEMP_DIR"
    exit 1
fi

# 检查安装脚本权限
if [ -x "$TEMP_DIR/v0.1.0/install.sh" ]; then
    echo "✅ 安装脚本具有执行权限"
else
    echo "❌ 安装脚本缺少执行权限"
    rm -rf "$TEMP_DIR"
    exit 1
fi

# 验证二进制文件校验和
echo "正在验证二进制文件校验和..."
cd "$TEMP_DIR/v0.1.0"
if sha256sum -c SHA256SUMS; then
    echo "✅ 二进制文件校验和验证通过"
else
    echo "❌ 二进制文件校验和验证失败"
    cd /workspace
    rm -rf "$TEMP_DIR"
    exit 1
fi

cd /workspace
rm -rf "$TEMP_DIR"

echo
echo "🎉 发布验证完成！所有检查都通过了。"
echo
echo "发布包信息:"
echo "- 文件名: $RELEASE_FILE"
echo "- 大小: $(ls -lh "$RELEASE_FILE" | awk '{print $5}')"
echo "- SHA256: $ACTUAL_SHA256"
echo
echo "发布包已准备就绪，可以上传到GitHub Releases。"