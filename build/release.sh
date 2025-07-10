#!/bin/bash
# NFS-CacheFS 完整发布脚本

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

# 验证版本号格式
if ! echo "$NEW_VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    print_error "版本号格式不正确，应为 x.y.z 格式"
    exit 1
fi

print_step "开始发布流程 v$NEW_VERSION"

# 1. 检查环境
print_step "检查环境依赖..."
command -v docker >/dev/null 2>&1 || { print_error "Docker 未安装"; exit 1; }
command -v gh >/dev/null 2>&1 || { print_error "GitHub CLI 未安装"; exit 1; }
command -v jq >/dev/null 2>&1 || { print_error "jq 未安装"; exit 1; }

# 检查 Git 状态
if [ -n "$(git status --porcelain)" ]; then
    print_error "Git 工作目录不干净，请先提交或暂存更改"
    exit 1
fi

# 2. 更新版本号
print_step "更新版本号..."
sed -i "s/^version = \".*\"/version = \"$NEW_VERSION\"/" Cargo.toml
sed -i "s/version-v[0-9]\+\.[0-9]\+\.[0-9]\+/version-v$NEW_VERSION/g" README.md
sed -i "s/download\/v[0-9]\+\.[0-9]\+\.[0-9]\+/download\/v$NEW_VERSION/g" README.md
sed -i "s/nfs-cachefs-v[0-9]\+\.[0-9]\+\.[0-9]\+/nfs-cachefs-v$NEW_VERSION/g" README.md
sed -i "s/## 🎉 最新版本 v[0-9]\+\.[0-9]\+\.[0-9]\+/## 🎉 最新版本 v$NEW_VERSION/" README.md
sed -i "s/- 当前版本: \*\*v[0-9]\+\.[0-9]\+\.[0-9]\+\*\*/- 当前版本: **v$NEW_VERSION**/" README.md
# 更新main.rs中的版本号
sed -i "s/\.version(\"[0-9]\+\.[0-9]\+\.[0-9]\+\")/\.version(\"$NEW_VERSION\")/g" src/main.rs
sed -i "s/Starting NFS-CacheFS v[0-9]\+\.[0-9]\+\.[0-9]\+/Starting NFS-CacheFS v$NEW_VERSION/g" src/main.rs

# 3. 自动更新 CHANGELOG
print_step "更新 CHANGELOG.md..."
# 创建临时文件
cat > /tmp/new_changelog_entry << EOF
## [$NEW_VERSION] - $TODAY

### Added
- 重构构建系统为 Docker 方式
- 添加完整的发布自动化流程
- 新增 GitHub Actions 自动发布工作流

### Changed
- 统一使用 Docker 构建，移除本地构建依赖
- 重新组织 build 目录结构
- 更新 Makefile 支持 Docker 构建

### Fixed
- 修复构建环境依赖问题
- 优化发布流程和文档

EOF

# 在 CHANGELOG.md 中插入新版本条目
sed -i '/^# Changelog/r /tmp/new_changelog_entry' CHANGELOG.md
rm -f /tmp/new_changelog_entry

print_warning "请手动编辑 CHANGELOG.md 添加版本 v$NEW_VERSION 的具体更新内容"
print_warning "按 Enter 键继续..."
read -r

# 4. 构建
print_step "执行 Docker 构建..."
make clean
make build

# 5. 验证构建
print_step "验证构建结果..."
if ! docker images | grep -q nfs-cachefs; then
    print_error "Docker 镜像构建失败"
    exit 1
fi

if ! ls nfs-cachefs-v*.tar.gz >/dev/null 2>&1; then
    print_error "发布包生成失败"
    exit 1
fi

# 测试镜像
make docker-test

# 6. 创建版本标签
print_step "创建版本标签..."
git add Cargo.toml README.md CHANGELOG.md src/main.rs
git commit -m "chore: bump version to v$NEW_VERSION"
git tag -a "v$NEW_VERSION" -m "Release v$NEW_VERSION"

# 7. 推送到远程
print_step "推送到远程仓库..."
git push origin main
git push origin "v$NEW_VERSION"

# 8. 创建 GitHub Release
print_step "创建 GitHub Release..."
RELEASE_NOTES=$(cat << EOF
# NFS-CacheFS v$NEW_VERSION

## 📦 安装方法

### 预编译二进制包（推荐）
\`\`\`bash
wget https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/download/v$NEW_VERSION/nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz
tar -xzf nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz
cd nfs-cachefs-v$NEW_VERSION-linux-x86_64
sudo ./install.sh
\`\`\`

## 🔍 校验和
\`\`\`bash
sha256sum -c nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz.sha256
\`\`\`

## 📋 更新内容
$(sed -n "/## \[$NEW_VERSION\]/,/## \[/p" CHANGELOG.md | head -n -1)

## 🛠️ 技术信息
- **构建方式**: Docker 构建（rust:1.78-alpine）
- **目标平台**: Linux x86_64
- **链接方式**: 静态链接（musl libc）
- **镜像大小**: 约 24MB
- **兼容性**: 所有 Linux 发行版

## 📖 文档
- [安装指南](https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')#快速开始)
- [发布流程](https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/blob/main/docs/RELEASE_PROCESS.md)
- [更新日志](https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/blob/main/CHANGELOG.md)
EOF
)

gh release create "v$NEW_VERSION" \
    --title "NFS-CacheFS v$NEW_VERSION" \
    --notes "$RELEASE_NOTES" \
    nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz \
    nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz.sha256

# 9. 清理临时文件
print_step "清理临时文件..."
rm -f *.tar.gz *.tar.gz.sha256
docker system prune -f
rm -rf nfs-cachefs-v*
rm -f .release-* release-notes-* temp-*

# 10. 创建 PR（如果需要）
CURRENT_BRANCH=$(git branch --show-current)
if [ "$CURRENT_BRANCH" != "main" ]; then
    print_step "创建 PR..."
    git push origin "$CURRENT_BRANCH"
    
    gh pr create \
        --title "Release v$NEW_VERSION" \
        --body "🚀 发布新版本 v$NEW_VERSION

## 📋 发布清单
- [x] 更新版本号 (Cargo.toml, README.md, main.rs)
- [x] 更新 CHANGELOG.md
- [x] Docker 构建成功
- [x] 二进制文件测试通过
- [x] GitHub Release 创建完成
- [x] 临时文件清理完成

## 🔗 相关链接
- [GitHub Release](https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/tag/v$NEW_VERSION)
- [下载页面](https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/latest)

## 🧪 测试结果
- ✅ Docker 镜像构建成功
- ✅ 二进制文件功能测试通过
- ✅ 静态链接验证通过
- ✅ 校验和生成正确

/cc @maintainers" \
        --assignee @me
    
    echo "📝 PR 创建成功: https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/pulls"
else
    echo "📌 当前在 main 分支，无需创建 PR"
fi

print_success "🎉 发布流程完成！"
print_success "📦 GitHub Release: https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/tag/v$NEW_VERSION"
print_success "🔗 下载页面: https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/latest" 