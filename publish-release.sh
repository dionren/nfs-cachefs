#!/bin/bash
# GitHub Release 发布脚本

set -e

echo "🚀 准备发布 NFS-CacheFS v0.4.0 到 GitHub Releases..."

# 检查必要文件
RELEASE_FILES=(
    "nfs-cachefs-v0.4.0-linux-x86_64.tar.gz"
    "nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256"
    "release-notes-v0.4.0.md"
)

echo "📋 检查发布文件..."
for file in "${RELEASE_FILES[@]}"; do
    if [[ -f "$file" ]]; then
        echo "  ✅ $file ($(du -h "$file" | cut -f1))"
    else
        echo "  ❌ $file - 文件不存在!"
        exit 1
    fi
done

echo ""
echo "🔧 选择发布方式:"
echo "1. 使用 GitHub CLI (需要认证)"
echo "2. 手动发布 (推荐)"
echo ""
read -p "请选择 (1/2): " choice

case $choice in
    1)
        echo ""
        echo "🔑 检查 GitHub CLI 认证状态..."
        if ! gh auth status > /dev/null 2>&1; then
            echo "⚠️  GitHub CLI 未认证，请先运行: gh auth login"
            exit 1
        fi
        
        echo "📤 使用 GitHub CLI 创建 Release..."
        gh release create v0.4.0 \
            --title "NFS-CacheFS v0.4.0" \
            --notes-file release-notes-v0.4.0.md \
            nfs-cachefs-v0.4.0-linux-x86_64.tar.gz \
            nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256
        
        echo "✅ Release 创建成功!"
        echo "🌐 查看: https://github.com/dionren/nfs-cachefs/releases/tag/v0.4.0"
        ;;
    2)
        echo ""
        echo "📖 手动发布指导:"
        echo ""
        echo "1. 访问: https://github.com/dionren/nfs-cachefs/releases/new"
        echo "2. Tag version: v0.4.0"
        echo "3. Release title: NFS-CacheFS v0.4.0"
        echo "4. 复制 release-notes-v0.4.0.md 的内容到描述框"
        echo "5. 上传文件:"
        echo "   - nfs-cachefs-v0.4.0-linux-x86_64.tar.gz"
        echo "   - nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256"
        echo "6. 勾选 'Set as the latest release'"
        echo "7. 点击 'Publish release'"
        echo ""
        echo "📄 发布说明内容已保存在: release-notes-v0.4.0.md"
        echo "📋 详细步骤请查看: create-github-release.md"
        ;;
    *)
        echo "❌ 无效选择"
        exit 1
        ;;
esac

echo ""
echo "🎉 发布流程完成!"