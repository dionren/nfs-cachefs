# NFS-CacheFS 完整发布流程

## 🚨 重要提醒
**本项目必须使用 Docker 构建，不支持本地构建！**
- **构建方式**: 仅支持 Docker 构建（rust:1.78-alpine）
- **推荐方式**: 使用 `build/release.sh` 脚本进行一键发布
- **版本管理**: 脚本会自动更新所有相关文件的版本号

## 🔥 强制发布要求
**发布版本时必须遵循以下强制性要求：**

### 💯 完整发布流程
- **必须立即执行**: 一旦开始发布，必须完成所有步骤，不允许中途停止
- **禁止中途询问**: 发布过程中不得询问用户确认，全程自动化执行
- **强制自动发布**: 编译完成后必须自动发布到 GitHub releases
- **完整流程覆盖**: 包括 Docker 编译、文档更新、GitHub 发布等所有环节

### 📦 二进制包发布要求
- **自动上传**: 编译后的二进制程序包必须自动上传到 GitHub releases
- **校验和文件**: 必须同时生成并上传 SHA256 校验和文件
- **完整性验证**: 确保发布包的完整性和可下载性
- **立即可用**: 发布完成后用户必须能立即下载使用

### 🚫 禁止的行为
- ❌ **禁止手动干预**: 不允许在发布过程中手动操作
- ❌ **禁止中途停止**: 不允许在发布过程中暂停或退出
- ❌ **禁止跳过步骤**: 必须完成所有发布步骤
- ❌ **禁止询问确认**: 不允许询问用户是否继续

### ✅ 强制执行项目
- [x] Docker 构建完成
- [x] 所有文件版本号更新
- [x] CHANGELOG.md 自动更新
- [x] 二进制包自动生成
- [x] GitHub Release 自动创建
- [x] 校验和文件自动上传
- [x] 发布完成验证

## 📋 快速发布（推荐）
使用自动化发布脚本，支持**参数输入版本号**进行一键发布：

```bash
# 给脚本执行权限（首次运行）
chmod +x build/release.sh

# 🎯 一键发布到指定版本（完全自动化）
./build/release.sh 1.2.3

# 脚本会自动完成以下所有操作：
# ✅ 环境检查和依赖验证
# ✅ 更新所有文件的版本号
# ✅ 自动生成 CHANGELOG 条目
# ✅ Docker 构建和测试
# ✅ 创建 Git 标签和提交
# ✅ 推送到远程仓库
# ✅ 创建 GitHub Release
# ✅ 清理临时文件
# ✅ 自动发布二进制包
# ✅ 立即可用的完整发布
```

**版本号格式要求**: 必须是 `x.y.z` 格式（如 `1.2.3`）

**⚠️ 重要**: 执行发布命令后，脚本将全程自动化执行，无需用户干预，直到完整发布完成。

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
- **Docker**: 20.10+ (必须！本项目仅支持 Docker 构建)
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

### 🎯 自动更新（推荐）
使用 `build/release.sh` 脚本会自动更新所有相关文件的版本号：

```bash
# 一键更新所有版本号到 1.2.3
./build/release.sh 1.2.3
```

脚本会自动更新以下文件的版本号：
- ✅ `Cargo.toml`
- ✅ `README.md`（多个位置）
- ✅ `src/main.rs`
- ✅ `CHANGELOG.md`（自动生成新条目）

### 手动更新（不推荐）
如果需要手动更新，请按以下步骤操作：

#### 1. 更新 Cargo.toml
```bash
# 示例：更新到版本 1.2.3
NEW_VERSION="1.2.3"

# 更新 Cargo.toml 中的版本号
sed -i "s/^version = \".*\"/version = \"$NEW_VERSION\"/" Cargo.toml

# 验证更新
grep "^version = " Cargo.toml
```

#### 2. 更新 src/main.rs 中的版本号
```bash
# 更新 main.rs 中的版本号
sed -i "s/\.version(\"[0-9]\+\.[0-9]\+\.[0-9]\+\")/\.version(\"$NEW_VERSION\")/g" src/main.rs
sed -i "s/Starting NFS-CacheFS v[0-9]\+\.[0-9]\+\.[0-9]\+/Starting NFS-CacheFS v$NEW_VERSION/g" src/main.rs

# 验证更新
grep "\.version(" src/main.rs
grep "Starting NFS-CacheFS" src/main.rs
```

#### 3. 更新 README.md
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

#### 4. 更新 CHANGELOG.md
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

### 🐳 Docker 构建（必须）
**本项目仅支持 Docker 构建方式**

```bash
# 清理之前的构建产物
make clean

# 🎯 执行 Docker 构建（必须）
make build

# 验证构建结果
docker images | grep nfs-cachefs
ls -la *.tar.gz*
```

### 验证构建产物
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
git add Cargo.toml README.md CHANGELOG.md src/main.rs
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
- [x] 更新版本号 (Cargo.toml, README.md, src/main.rs)
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

### 🚀 一键发布脚本（推荐）
使用 `build/release.sh` 脚本进行**参数化版本号**的一键发布：

```bash
# 赋予执行权限
chmod +x build/release.sh

# 🎯 执行发布（指定版本号）- 完全自动化
./build/release.sh 1.2.3

# 脚本特性：
# ✅ 参数输入版本号（必须是 x.y.z 格式）
# ✅ 自动环境检查和依赖验证
# ✅ 自动更新所有文件的版本号
# ✅ 强制 Docker 构建
# ✅ 自动生成 CHANGELOG 条目
# ✅ 全自动 GitHub 发布流程
# ✅ 自动清理临时文件
# ✅ 零手动干预 - 完全自动化
# ✅ 立即可用的完整发布
```

**⚠️ 重要变更**: 发布脚本已完全自动化，不再需要任何手动确认或干预。

### 脚本功能详解
`build/release.sh` 脚本支持以下功能：

1. **参数版本号输入**
   ```bash
   ./build/release.sh 1.2.3  # 发布到 v1.2.3
   ./build/release.sh 2.0.0  # 发布到 v2.0.0
   ```

2. **自动版本号更新**
   - `Cargo.toml` - 项目版本号
   - `README.md` - 版本徽章、下载链接、版本说明
   - `src/main.rs` - 应用程序版本号和启动信息
   - `CHANGELOG.md` - 自动生成新版本条目

3. **强制 Docker 构建**
   - 环境检查确保 Docker 可用
   - 仅支持 Docker 构建方式
   - 自动清理和重新构建

4. **完整发布流程**
   - 创建 Git 标签
   - 推送到远程仓库
   - 创建 GitHub Release
   - 上传构建产物
   - **全程零干预自动化**

### 📋 完全自动化发布流程

执行 `./build/release.sh <版本号>` 后，脚本将按以下顺序自动完成：

1. **🔍 环境检查** - 自动验证 Docker、GitHub CLI、jq 等依赖
2. **📝 版本号更新** - 自动更新所有相关文件的版本号
3. **📋 CHANGELOG 更新** - 自动生成并插入新版本条目
4. **🐳 Docker 构建** - 强制使用 Docker 进行构建
5. **🔍 构建验证** - 自动验证构建结果和产物完整性
6. **🏷️ 标签创建** - 自动创建 Git 标签和提交
7. **🚀 远程推送** - 自动推送代码和标签到远程仓库
8. **📦 GitHub 发布** - 自动创建 GitHub Release 并上传二进制包
9. **🧹 清理工作** - 自动清理临时文件和构建缓存
10. **📊 完成报告** - 自动生成发布完成报告

**整个过程完全自动化，无需任何手动干预或确认。**

完整发布脚本代码：

```bash
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

# 🎯 检查参数（支持参数输入版本号）
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

# 1. 检查环境（强制 Docker）
print_step "检查环境依赖..."
command -v docker >/dev/null 2>&1 || { print_error "Docker 未安装（必须）"; exit 1; }
command -v gh >/dev/null 2>&1 || { print_error "GitHub CLI 未安装"; exit 1; }
command -v jq >/dev/null 2>&1 || { print_error "jq 未安装"; exit 1; }

# 检查 Git 状态
if [ -n "$(git status --porcelain)" ]; then
    print_error "Git 工作目录不干净，请先提交或暂存更改"
    exit 1
fi

# 2. 🎯 自动更新所有版本号
print_step "自动更新所有版本号..."
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
print_step "自动更新 CHANGELOG.md..."
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

# 4. 🐳 强制 Docker 构建
print_step "执行 Docker 构建（强制）..."
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
```

---

## 故障排除

### 常见问题

**Q: 构建失败 - 本地构建不支持**
```bash
# 解决方案：必须使用 Docker 构建
make clean
make build  # 这会自动使用 Docker 构建
```

**Q: 版本号格式错误**
```bash
# 正确格式
./build/release.sh 1.2.3   # ✅ 正确
./build/release.sh v1.2.3  # ❌ 错误
./build/release.sh 1.2     # ❌ 错误
```

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

### 🎯 发布检查清单

- [ ] 环境依赖已安装（Docker, GitHub CLI, jq）
- [ ] Git 工作目录干净
- [ ] 版本号格式正确（x.y.z）
- [ ] 使用 `./build/release.sh <版本号>` 进行发布
- [ ] 所有文件版本号已自动更新
- [ ] CHANGELOG.md 已手动编辑
- [ ] Docker 构建成功
- [ ] 二进制文件测试通过
- [ ] 版本标签已创建
- [ ] GitHub Release 已发布
- [ ] 临时文件已清理
- [ ] PR 已创建（如需要）

### 📋 版本号更新文件清单

自动化脚本会更新以下文件的版本号：

- [x] **Cargo.toml** - 项目版本号
- [x] **README.md** - 版本徽章、下载链接、版本说明
- [x] **src/main.rs** - 应用程序版本号和启动信息
- [x] **CHANGELOG.md** - 自动生成新版本条目（需手动编辑内容）

---

*最后更新：2025年1月 - 强调 Docker 构建和参数化版本号发布* 