#!/bin/bash
# 本地 Rust 编译脚本

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# 进入项目根目录
cd "$(dirname "$0")/.."

# 获取版本信息
VERSION=$(grep '^version = ' Cargo.toml | sed 's/version = "\(.*\)"/\1/')
echo -e "${GREEN}构建 NFS-CacheFS 版本 ${VERSION}...${NC}"

# 检查 Rust 环境
echo -e "${YELLOW}检查 Rust 编译环境...${NC}"
if ! command -v cargo &> /dev/null; then
    echo -e "${RED}错误: 未找到 cargo，请先安装 Rust${NC}"
    echo "安装方法: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

if ! command -v rustc &> /dev/null; then
    echo -e "${RED}错误: 未找到 rustc，请先安装 Rust${NC}"
    exit 1
fi

# 显示 Rust 版本信息
echo "Rust 版本: $(rustc --version)"
echo "Cargo 版本: $(cargo --version)"

# 检查系统依赖
echo -e "${YELLOW}检查系统依赖...${NC}"
MISSING_DEPS=""

# 检查 FUSE 开发库
if ! pkg-config --exists fuse3 2>/dev/null && ! pkg-config --exists fuse 2>/dev/null; then
    MISSING_DEPS="$MISSING_DEPS libfuse-dev (或 libfuse3-dev)"
fi

# 检查构建工具
if ! command -v pkg-config &> /dev/null; then
    MISSING_DEPS="$MISSING_DEPS pkg-config"
fi

if [ ! -z "$MISSING_DEPS" ]; then
    echo -e "${RED}错误: 缺少以下依赖:${NC}"
    echo "$MISSING_DEPS"
    echo ""
    echo "安装方法:"
    echo "  Ubuntu/Debian: sudo apt-get install libfuse3-dev pkg-config"
    echo "  CentOS/RHEL:   sudo yum install fuse3-devel pkgconfig"
    echo "  Fedora:        sudo dnf install fuse3-devel pkgconfig"
    echo "  Arch:          sudo pacman -S fuse3 pkgconf"
    exit 1
fi

# 创建发布目录
RELEASE_DIR="nfs-cachefs-v${VERSION}-linux-x86_64"
rm -rf "${RELEASE_DIR}"
mkdir -p "${RELEASE_DIR}"

# 编译选项
BUILD_MODE="release"
CARGO_FLAGS="--release"

# 解析命令行参数
while [[ $# -gt 0 ]]; do
    case $1 in
        --debug)
            BUILD_MODE="debug"
            CARGO_FLAGS=""
            shift
            ;;
        --features)
            CARGO_FLAGS="$CARGO_FLAGS --features $2"
            shift 2
            ;;
        --io-uring)
            CARGO_FLAGS="$CARGO_FLAGS --features io_uring"
            echo -e "${GREEN}启用 io_uring 支持${NC}"
            shift
            ;;
        --help|-h)
            echo "用法: $0 [选项]"
            echo ""
            echo "选项:"
            echo "  --debug          构建调试版本"
            echo "  --io-uring       启用 io_uring 支持"
            echo "  --features <f>   启用指定特性"
            echo "  --help, -h       显示此帮助信息"
            exit 0
            ;;
        *)
            echo "未知参数: $1"
            echo "用法: $0 [--debug] [--io-uring] [--features <features>]"
            exit 1
            ;;
    esac
done

# 清理之前的构建
echo -e "${YELLOW}清理之前的构建产物...${NC}"
cargo clean

# 开始编译
echo -e "${GREEN}开始编译 (${BUILD_MODE} 模式)...${NC}"
RUSTFLAGS="-C target-cpu=native" cargo build $CARGO_FLAGS

# 检查编译结果
if [ $? -ne 0 ]; then
    echo -e "${RED}编译失败！${NC}"
    exit 1
fi

# 复制二进制文件
echo -e "${YELLOW}复制二进制文件...${NC}"
if [ "$BUILD_MODE" = "release" ]; then
    cp "target/release/nfs-cachefs" "${RELEASE_DIR}/"
    strip "${RELEASE_DIR}/nfs-cachefs"  # 去除调试符号
else
    cp "target/debug/nfs-cachefs" "${RELEASE_DIR}/"
fi

# 验证二进制文件
if ! ldd "${RELEASE_DIR}/nfs-cachefs" 2>/dev/null | grep -q "not a dynamic executable"; then
    echo -e "${YELLOW}注意: 生成的是动态链接的二进制文件${NC}"
    echo "依赖库:"
    ldd "${RELEASE_DIR}/nfs-cachefs" | grep -v "linux-vdso"
fi

# 复制文档和配置文件
echo -e "${YELLOW}复制文档和配置文件...${NC}"
cp README.md "${RELEASE_DIR}/" 2>/dev/null || true
cp LICENSE "${RELEASE_DIR}/" 2>/dev/null || true
cp CHANGELOG.md "${RELEASE_DIR}/" 2>/dev/null || true
cp CLAUDE.md "${RELEASE_DIR}/" 2>/dev/null || true
cp UPGRADE_PLAN.md "${RELEASE_DIR}/" 2>/dev/null || true
cp build/install.sh "${RELEASE_DIR}/" 2>/dev/null || true

# 创建 docs 目录
if [ -d "docs" ]; then
    mkdir -p "${RELEASE_DIR}/docs"
    cp -r docs/* "${RELEASE_DIR}/docs/" 2>/dev/null || true
fi

# 创建使用说明
cat > "${RELEASE_DIR}/USAGE.md" << 'EOF'
# NFS-CacheFS 使用说明

## 快速开始

### 1. 安装
```bash
sudo ./install.sh
```

### 2. 基本使用
```bash
# 查看帮助
nfs-cachefs --help

# 挂载示例
sudo nfs-cachefs /mnt/nfs /mnt/cached -o cache_dir=/mnt/cache,cache_size_gb=50

# 使用 mount 命令挂载
sudo mount -t cachefs -o nfs_backend=/mnt/nfs,cache_dir=/mnt/cache,cache_size_gb=50 cachefs /mnt/cached
```

### 3. 配置选项
- `nfs_backend`: NFS 后端路径（必需）
- `cache_dir`: 本地缓存目录（必需）
- `cache_size_gb`: 缓存大小（GB）
- `block_size_mb`: 块大小（MB，默认64）
- `min_cache_file_size_mb`: 最小缓存文件大小（MB，默认100）

### 4. 性能优化
- 使用 NVMe SSD 作为缓存目录
- 适当增大 block_size_mb 以提高大文件性能
- 调整 min_cache_file_size_mb 以优化缓存策略

## 注意事项
- 需要 root 权限运行
- 确保 NFS 已正确挂载
- 缓存目录需要足够的空间
EOF

# 运行测试（可选）
if [ "$BUILD_MODE" = "debug" ]; then
    echo -e "${YELLOW}运行测试...${NC}"
    cargo test
fi

# 创建压缩包
echo -e "${YELLOW}创建发布包...${NC}"
tar -czf "${RELEASE_DIR}.tar.gz" "${RELEASE_DIR}"

# 生成校验和
echo -e "${YELLOW}生成校验和...${NC}"
sha256sum "${RELEASE_DIR}.tar.gz" > "${RELEASE_DIR}.tar.gz.sha256"

# 显示二进制文件信息
echo -e "${GREEN}二进制文件信息:${NC}"
file "${RELEASE_DIR}/nfs-cachefs"
ls -lh "${RELEASE_DIR}/nfs-cachefs"

# 清理临时目录（可选）
# rm -rf "${RELEASE_DIR}"

echo ""
echo -e "${GREEN}✅ 本地编译完成!${NC}"
echo ""
echo -e "${GREEN}📦 生成的文件:${NC}"
echo "  - 二进制文件: ${RELEASE_DIR}/nfs-cachefs"
echo "  - 发布包: ${RELEASE_DIR}.tar.gz"
echo "  - 校验和: ${RELEASE_DIR}.tar.gz.sha256"
echo ""
echo -e "${GREEN}🚀 使用方法:${NC}"
echo "  1. 解压并安装:"
echo "     tar -xzf ${RELEASE_DIR}.tar.gz"
echo "     cd ${RELEASE_DIR}"
echo "     sudo ./install.sh"
echo ""
echo "  2. 或直接运行:"
echo "     ./${RELEASE_DIR}/nfs-cachefs --help"
echo ""
echo -e "${YELLOW}📖 详细使用说明请参考 ${RELEASE_DIR}/USAGE.md${NC}"