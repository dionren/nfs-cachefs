# NFS-CacheFS 完整发布流程

## 目录
1. [发布准备](#发布准备)
2. [版本号更新](#版本号更新)
3. [构建和打包](#构建和打包)
4. [GitHub 发布](#github-发布)
5. [清理和提交](#清理和提交)
6. [自动化脚本](#自动化脚本)
7. [故障排除](#故障排除)

---

## 发布准备

### 环境要求
- **Docker**: 20.10+ (用于构建)
- **Git**: 2.20+ (用于版本控制)
- **GitHub CLI**: 2.0+ (用于自动发布)
- **jq**: 1.6+ (用于 JSON 处理)

### 环境安装
```bash
# 安装 GitHub CLI
curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | sudo dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg
echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" | sudo tee /etc/apt/sources.list.d/github-cli.list > /dev/null
sudo apt update
sudo apt install gh jq

# 登录 GitHub
gh auth login
```

---

## 版本号更新

### 1. 更新 Cargo.toml
```bash
# 示例：更新到版本 1.2.3
NEW_VERSION="1.2.3"

# 更新 Cargo.toml 中的版本号
sed -i "s/^version = \".*\"/version = \"$NEW_VERSION\"/" Cargo.toml

# 验证更新
grep "^version = " Cargo.toml
```

### 2. 更新 README.md
```bash
# 更新 README.md 中的版本徽章和下载链接
sed -i "s/version-v[0-9]\+\.[0-9]\+\.[0-9]\+/version-v$NEW_VERSION/g" README.md
sed -i "s/download\/v[0-9]\+\.[0-9]\+\.[0-9]\+/download\/v$NEW_VERSION/g" README.md
sed -i "s/nfs-cachefs-v[0-9]\+\.[0-9]\+\.[0-9]\+/nfs-cachefs-v$NEW_VERSION/g" README.md

# 更新版本发布日期
TODAY=$(date +%Y-%m-%d)
sed -i "s/## 🎉 最新版本 v[0-9]\+\.[0-9]\+\.[0-9]\+/## 🎉 最新版本 v$NEW_VERSION/" README.md
sed -i "s/- 当前版本: \*\*v[0-9]\+\.[0-9]\+\.[0-9]\+\*\*/- 当前版本: **v$NEW_VERSION**/" README.md
sed -i "s/([0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\})/($TODAY)/" README.md
```

### 3. 更新 CHANGELOG.md
```bash
# 创建新版本条目
NEW_CHANGELOG_ENTRY="## [$NEW_VERSION] - $TODAY

### Added
- 新功能说明（请手动编辑）

### Changed
- 变更说明（请手动编辑）

### Fixed
- 修复说明（请手动编辑）

"

# 在 CHANGELOG.md 中插入新版本条目
sed -i "/^# Changelog/a\\
\\
$NEW_CHANGELOG_ENTRY" CHANGELOG.md

echo "⚠️  请手动编辑 CHANGELOG.md 添加具体的更新内容"
```

---

## 构建和打包

### 1. Docker 构建
```bash
# 清理之前的构建产物
make clean

# 执行 Docker 构建
make build

# 验证构建结果
docker images | grep nfs-cachefs
ls -la *.tar.gz*
```

### 2. 验证构建产物
```bash
# 测试 Docker 镜像
make docker-test

# 测试二进制文件
tar -xzf nfs-cachefs-v*.tar.gz
cd nfs-cachefs-v*
./nfs-cachefs --version
cd ..
rm -rf nfs-cachefs-v*
```

---

## GitHub 发布

### 1. 创建版本标签
```bash
# 提交版本更新
git add Cargo.toml README.md CHANGELOG.md
git commit -m "chore: bump version to v$NEW_VERSION"

# 创建版本标签
git tag -a "v$NEW_VERSION" -m "Release v$NEW_VERSION"

# 推送到远程仓库
git push origin main
git push origin "v$NEW_VERSION"
```

### 2. 创建 GitHub Release
```bash
# 生成发布说明
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

### Docker 镜像
\`\`\`bash
docker pull nfs-cachefs:$NEW_VERSION
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

# 创建 GitHub Release
gh release create "v$NEW_VERSION" \
    --title "NFS-CacheFS v$NEW_VERSION" \
    --notes "$RELEASE_NOTES" \
    nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz \
    nfs-cachefs-v$NEW_VERSION-linux-x86_64.tar.gz.sha256

echo "✅ GitHub Release 创建成功: https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/tag/v$NEW_VERSION"
```

---

## 清理和提交

### 1. 清理临时文件
```bash
# 清理构建产物
rm -f *.tar.gz *.tar.gz.sha256

# 清理 Docker 缓存
docker system prune -f

# 清理解压的临时目录
rm -rf nfs-cachefs-v*

# 清理其他临时文件
rm -f .release-*
rm -f release-notes-*
rm -f temp-*

echo "🧹 临时文件清理完成"
```

### 2. 创建 PR（如果在功能分支）
```bash
# 如果在功能分支，创建 PR
CURRENT_BRANCH=$(git branch --show-current)
if [ "$CURRENT_BRANCH" != "main" ]; then
    # 推送当前分支
    git push origin "$CURRENT_BRANCH"
    
    # 创建 PR
    gh pr create \
        --title "Release v$NEW_VERSION" \
        --body "🚀 发布新版本 v$NEW_VERSION

## 📋 发布清单
- [x] 更新版本号 (Cargo.toml, README.md)
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
```

---

## 自动化脚本

### 完整发布脚本
创建 `build/release.sh` 脚本：

```bash
#!/bin/bash
# 完整发布脚本

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

# 3. 提示更新 CHANGELOG
print_warning "请手动编辑 CHANGELOG.md 添加版本 v$NEW_VERSION 的更新内容"
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
git add Cargo.toml README.md CHANGELOG.md
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
- [x] 更新版本号 (Cargo.toml, README.md)
- [x] 更新 CHANGELOG.md
- [x] Docker 构建成功
- [x] 二进制文件测试通过
- [x] GitHub Release 创建完成
- [x] 临时文件清理完成

## 🔗 相关链接
- [GitHub Release](https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/tag/v$NEW_VERSION)
- [下载页面](https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/latest)" \
        --assignee @me
fi

print_success "🎉 发布流程完成！"
print_success "📦 GitHub Release: https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/tag/v$NEW_VERSION"
print_success "🔗 下载页面: https://github.com/$(gh repo view --json owner,name -q '.owner.login + "/" + .name')/releases/latest"
```

### 使用自动化脚本
```bash
# 赋予执行权限
chmod +x build/release.sh

# 执行发布
./build/release.sh 1.2.3
```

---

## 故障排除

### 常见问题

**Q: GitHub CLI 认证失败**
```bash
# 重新登录
gh auth logout
gh auth login --web
```

**Q: Docker 构建失败**
```bash
# 清理 Docker 环境
docker system prune -a -f
docker builder prune -a -f

# 重新构建
make build
```

**Q: 版本标签已存在**
```bash
# 删除本地标签
git tag -d v1.2.3

# 删除远程标签
git push origin --delete v1.2.3
```

**Q: GitHub Release 创建失败**
```bash
# 检查权限
gh auth status

# 手动创建 Release
gh release create v1.2.3 \
    --title "NFS-CacheFS v1.2.3" \
    --notes "Release notes here" \
    *.tar.gz*
```

### 发布检查清单

- [ ] 环境依赖已安装（Docker, GitHub CLI, jq）
- [ ] Git 工作目录干净
- [ ] 版本号格式正确
- [ ] CHANGELOG.md 已更新
- [ ] Docker 构建成功
- [ ] 二进制文件测试通过
- [ ] 版本标签已创建
- [ ] GitHub Release 已发布
- [ ] 临时文件已清理
- [ ] PR 已创建（如需要）

---

*最后更新：2025年7月* 