#!/bin/bash

# 完成发布流程的脚本

set -e

VERSION="0.5.2"
echo "完成 NFS-CacheFS v${VERSION} 的发布流程..."

# 1. 提交版本更新
echo "提交版本更新..."
git add .
git commit -m "chore: bump version to v${VERSION}"

# 2. 创建版本标签
echo "创建版本标签..."
git tag -a "v${VERSION}" -m "Release v${VERSION}"

# 3. 推送到远程
echo "推送到远程仓库..."
git push origin main
git push origin "v${VERSION}"

# 4. 创建 GitHub Release
echo "创建 GitHub Release..."
gh release create "v${VERSION}" \
    --title "NFS-CacheFS v${VERSION}" \
    --notes "# NFS-CacheFS v${VERSION}

## 📦 安装方法

### 预编译二进制包（推荐）
\`\`\`bash
wget https://github.com/dionren/nfs-cachefs/releases/download/v${VERSION}/nfs-cachefs-v${VERSION}-linux-x86_64.tar.gz
tar -xzf nfs-cachefs-v${VERSION}-linux-x86_64.tar.gz
cd nfs-cachefs-v${VERSION}-linux-x86_64
sudo ./install.sh
\`\`\`

## 🔍 校验和
\`\`\`bash
sha256sum -c nfs-cachefs-v${VERSION}-linux-x86_64.tar.gz.sha256
\`\`\`

## 📋 更新内容

### Added
- 完整的自动化发布流程和脚本
- 详细的发布流程文档 (RELEASE_PROCESS.md)
- 自动化版本号更新功能

### Changed
- 改进 Docker 构建系统的稳定性
- 优化 release.sh 脚本的错误处理
- 统一发布包命名和版本管理

### Fixed
- 修复发布脚本中的版本号同步问题
- 改进构建产物的清理和验证流程
- 优化发布流程中的依赖检查

## 🛠️ 技术信息
- **构建方式**: Docker 构建（rust:1.78-alpine）
- **目标平台**: Linux x86_64
- **链接方式**: 静态链接（musl libc）
- **镜像大小**: 约 24MB
- **兼容性**: 所有 Linux 发行版

## 📖 文档
- [安装指南](https://github.com/dionren/nfs-cachefs#快速开始)
- [发布流程](https://github.com/dionren/nfs-cachefs/blob/main/docs/RELEASE_PROCESS.md)
- [更新日志](https://github.com/dionren/nfs-cachefs/blob/main/CHANGELOG.md)" \
    nfs-cachefs-v${VERSION}-linux-x86_64.tar.gz \
    nfs-cachefs-v${VERSION}-linux-x86_64.tar.gz.sha256

echo "🎉 发布完成！"
echo "📦 GitHub Release: https://github.com/dionren/nfs-cachefs/releases/tag/v${VERSION}"
echo "🔗 下载页面: https://github.com/dionren/nfs-cachefs/releases/latest" 