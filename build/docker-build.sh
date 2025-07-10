#!/bin/bash
# Docker 构建脚本 - 使用 rust:1.78-alpine 镜像

set -e

# 进入项目根目录
cd "$(dirname "$0")/.."

# 获取版本信息
VERSION=$(grep '^version = ' Cargo.toml | sed 's/version = "\(.*\)"/\1/')
echo "使用 Docker 构建 NFS-CacheFS 版本 ${VERSION}..."

# 构建 Docker 镜像
echo "正在构建 Docker 镜像..."
docker build -f build/Dockerfile -t nfs-cachefs:${VERSION} .
docker tag nfs-cachefs:${VERSION} nfs-cachefs:latest

echo "Docker 镜像构建完成!"

# 创建临时容器来提取二进制文件
echo "从 Docker 镜像中提取二进制文件..."
CONTAINER_ID=$(docker create nfs-cachefs:${VERSION})

# 创建发布目录
RELEASE_DIR="nfs-cachefs-v${VERSION}-linux-x86_64"
rm -rf "${RELEASE_DIR}"
mkdir -p "${RELEASE_DIR}"

# 从容器中复制二进制文件
docker cp "${CONTAINER_ID}:/usr/local/bin/nfs-cachefs" "${RELEASE_DIR}/"

# 清理临时容器
docker rm "${CONTAINER_ID}"

# 复制其他文件
echo "复制文档和配置文件..."
cp README.md "${RELEASE_DIR}/"
cp LICENSE "${RELEASE_DIR}/"
cp CHANGELOG.md "${RELEASE_DIR}/"
cp build/install.sh "${RELEASE_DIR}/"

# 创建 docs 目录
mkdir -p "${RELEASE_DIR}/docs"
cp -r docs/* "${RELEASE_DIR}/docs/" 2>/dev/null || true

# 创建 Docker 使用说明
cat > "${RELEASE_DIR}/DOCKER_USAGE.md" << 'EOF'
# Docker 使用说明

## 构建镜像
```bash
docker build -t nfs-cachefs .
```

## 运行容器
```bash
# 基本运行
docker run --rm nfs-cachefs --help

# 挂载 NFS 缓存（需要特权模式）
docker run --rm --privileged \
  -v /path/to/nfs:/mnt/nfs \
  -v /path/to/cache:/mnt/cache \
  -v /path/to/mount:/mnt/cached \
  nfs-cachefs \
  --nfs-backend /mnt/nfs \
  --cache-dir /mnt/cache \
  --mount-point /mnt/cached \
  --cache-size-gb 50
```

## 注意事项
- 需要使用 `--privileged` 模式才能挂载文件系统
- 需要正确映射主机目录到容器内的挂载点
- 确保主机上已经挂载了 NFS 文件系统
EOF

# 创建压缩包
echo "创建发布包..."
tar -czf "${RELEASE_DIR}.tar.gz" "${RELEASE_DIR}"

# 生成校验和
echo "生成校验和..."
sha256sum "${RELEASE_DIR}.tar.gz" > "${RELEASE_DIR}.tar.gz.sha256"

# 清理临时目录
rm -rf "${RELEASE_DIR}"

echo ""
echo "✅ Docker 构建完成!"
echo ""
echo "📦 生成的文件:"
echo "  - Docker 镜像: nfs-cachefs:${VERSION} 和 nfs-cachefs:latest"
echo "  - 发布包: ${RELEASE_DIR}.tar.gz"
echo "  - 校验和: ${RELEASE_DIR}.tar.gz.sha256"
echo ""
echo "🚀 使用方法:"
echo "  1. 运行 Docker 容器:"
echo "     docker run --rm nfs-cachefs:${VERSION} --help"
echo ""
echo "  2. 或者使用提取的二进制文件:"
echo "     tar -xzf ${RELEASE_DIR}.tar.gz"
echo "     cd ${RELEASE_DIR}"
echo "     ./install.sh"
echo ""
echo "📖 详细使用说明请参考 DOCKER_USAGE.md" 