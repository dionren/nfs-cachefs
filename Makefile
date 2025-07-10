# NFS-CacheFS Makefile - Docker 构建方式

.PHONY: build release clean help test docker-build docker-test auto-release

# Default target
help:
	@echo "NFS-CacheFS - Docker 构建方式"
	@echo ""
	@echo "可用目标:"
	@echo "  build         - 使用 Docker 构建发布版本"
	@echo "  release       - 同 build（构建发布版本）"
	@echo "  docker-build  - 同 build（构建 Docker 镜像和发布包）"
	@echo "  docker-test   - 测试 Docker 镜像功能"
	@echo "  test          - 运行测试（在 Docker 容器中）"
	@echo "  auto-release  - 自动发布新版本（需要版本号参数）"
	@echo "  clean         - 清理构建产物"
	@echo "  help          - 显示此帮助信息"
	@echo ""
	@echo "构建文件位置:"
	@echo "  Docker 构建脚本:     ./build/docker-build.sh"
	@echo "  Dockerfile:         ./build/Dockerfile"
	@echo "  安装脚本:           ./build/install.sh"
	@echo "  发布流程文档:       ./docs/RELEASE_PROCESS.md"

# Docker 构建（推荐用于生产环境）
build release docker-build:
	@echo "使用 Docker 构建 NFS-CacheFS..."
	./build/docker-build.sh

# 在 Docker 容器中运行测试
test:
	@echo "在 Docker 容器中运行测试..."
	docker run --rm -v $(PWD):/app -w /app rust:1.78-alpine sh -c "\
		apk add --no-cache musl-dev pkgconfig fuse3-dev build-base linux-headers && \
		cargo test --release"

# 测试 Docker 镜像功能
docker-test:
	@echo "测试 Docker 镜像功能..."
	@if ! docker images | grep -q nfs-cachefs; then \
		echo "错误: 未找到 nfs-cachefs Docker 镜像，请先运行 'make build'"; \
		exit 1; \
	fi
	@echo "测试镜像版本信息..."
	docker run --rm nfs-cachefs:latest --version
	@echo "测试镜像帮助信息..."
	docker run --rm nfs-cachefs:latest --help

# 清理构建产物
clean:
	@echo "清理构建产物..."
	rm -f *.tar.gz *.tar.gz.sha256
	docker system prune -f
	@echo "清理完成"

# 显示构建信息
info:
	@echo "项目信息:"
	@echo "  项目名称: nfs-cachefs"
	@echo "  构建方式: Docker 构建（rust:1.78-alpine）"
	@echo "  目标平台: Linux x86_64"
	@echo "  链接方式: 静态链接（musl libc）"
	@echo ""
	@echo "Docker 镜像:"
	@if docker images | grep -q nfs-cachefs; then \
		docker images | grep nfs-cachefs; \
	else \
		echo "  未找到 nfs-cachefs 镜像，请运行 'make build'"; \
	fi
	@echo ""
	@echo "发布包:"
	@if ls *.tar.gz >/dev/null 2>&1; then \
		ls -la *.tar.gz*; \
	else \
		echo "  未找到发布包，请运行 'make build'"; \
	fi

# 自动发布新版本
auto-release:
	@if [ -z "$(VERSION)" ]; then \
		echo "错误: 请指定版本号"; \
		echo "用法: make auto-release VERSION=1.2.3"; \
		exit 1; \
	fi
	@echo "开始自动发布版本 $(VERSION)..."
	./build/release.sh $(VERSION) 