#!/bin/bash
# 非交互式发布脚本

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

print_step() {
    echo -e "${BLUE}[STEP]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# 检查参数
if [ $# -ne 1 ]; then
    print_error "用法: $0 <新版本号>"
    print_error "示例: $0 1.2.3"
    exit 1
fi

NEW_VERSION="$1"
TODAY=$(date +%Y-%m-%d)

print_step "开始发布流程 v$NEW_VERSION"

# 1. 检查环境
print_step "检查环境依赖..."
command -v docker >/dev/null 2>&1 || { print_error "Docker 未安装"; exit 1; }
command -v gh >/dev/null 2>&1 || { print_error "GitHub CLI 未安装"; exit 1; }
command -v jq >/dev/null 2>&1 || { print_error "jq 未安装"; exit 1; }

# 2. 添加并提交所有更改
print_step "提交版本更新..."
git add .
git commit -m "chore: bump version to v$NEW_VERSION"

# 3. 构建
print_step "执行 Docker 构建..."
make clean
make build

# 4. 验证构建
print_step "验证构建结果..."
if ! docker images | grep -q nfs-cachefs; then
    print_error "Docker 镜像构建失败"
    exit 1
fi

if ! ls nfs-cachefs-v*.tar.gz >/dev/null 2>&1; then
    print_error "发布包生成失败"
    exit 1
fi

# 5. 创建版本标签
print_step "创建版本标签..."
git tag -a "v$NEW_VERSION" -m "Release v$NEW_VERSION"

# 6. 推送到远程
print_step "推送到远程仓库..."
git push origin main
git push origin "v$NEW_VERSION"

# 7. 创建 GitHub Release
print_step "创建 GitHub Release..."
RELEASE_NOTES=$(cat << EOF
# NFS-CacheFS v$NEW_VERSION

## 📦 安装方法

### 预编译二进制包（推荐）
\`\`\`bash
wget https://github.com/\$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/download/v$NEW_VERSION/nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz
tar -xzf nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz
cd nfs-cachefs-v$NEW_VERSION-linux-x86_64
sudo ./install.sh
\`\`\`

## 🔍 校验和
\`\`\`bash
sha256sum -c nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz.sha256
\`\`\`

## 📋 更新内容
\$(sed -n "/## \[$NEW_VERSION\]/,/## \[/p" CHANGELOG.md | head -n -1)

## 🛠️ 技术信息
- **构建方式**: Docker 构建（rust:1.78-alpine）
- **目标平台**: Linux x86_64
- **链接方式**: 静态链接（musl libc）
- **镜像大小**: 约 24MB
- **兼容性**: 所有 Linux 发行版

## 📖 文档
- [安装指南](https://github.com/\$(gh repo view --json owner,name -q '.owner.login + "/" + .name')#快速开始)
- [发布流程](https://github.com/\$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/blob/main/docs/RELEASE_PROCESS.md)
- [更新日志](https://github.com/\$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/blob/main/CHANGELOG.md)
EOF
)

gh release create "v$NEW_VERSION" \
    --title "NFS-CacheFS v$NEW_VERSION" \
    --notes "$RELEASE_NOTES" \
    nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz \
    nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz.sha256

# 8. 清理临时文件
print_step "清理临时文件..."
rm -f *.tar.gz *.tar.gz.sha256
docker system prune -f
rm -rf nfs-cachefs-v*
rm -f .release-* release-notes-* temp-*

print_success "🎉 发布流程完成！"
print_success "📦 GitHub Release: https://github.com/\$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/tag/v$NEW_VERSION"
print_success "🔗 下载页面: https://github.com/\$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/latest" 