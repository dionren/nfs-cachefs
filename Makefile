# NFS-CacheFS Makefile - 本地 Rust 编译方式

.PHONY: build release clean help test debug install check-deps docker-build auto-release

# Default target
help:
	@echo "NFS-CacheFS - 本地 Rust 编译方式"
	@echo ""
	@echo "可用目标:"
	@echo "  build         - 使用本地 Rust 环境构建发布版本"
	@echo "  build-io-uring- 构建带 io_uring 支持的版本"
	@echo "  debug         - 构建调试版本"
	@echo "  debug-io-uring- 构建调试版本 (带 io_uring)"
	@echo "  release       - 同 build（构建发布版本）"
	@echo "  test          - 运行测试"
	@echo "  install       - 安装到系统"
	@echo "  clean         - 清理构建产物"
	@echo "  check-deps    - 检查依赖"
	@echo "  docker-build  - 使用 Docker 构建（备用）"
	@echo "  auto-release  - 自动发布新版本（需要版本号参数）"
	@echo "  help          - 显示此帮助信息"
	@echo ""
	@echo "构建文件位置:"
	@echo "  本地构建脚本:       ./build/local-build.sh"
	@echo "  Docker 构建脚本:    ./build/docker-build.sh（备用）"
	@echo "  安装脚本:           ./build/install.sh"
	@echo "  升级计划:           ./UPGRADE_PLAN.md"

# 本地构建（推荐）
build release:
	@echo "使用本地 Rust 环境构建 NFS-CacheFS..."
	./build/local-build.sh

# 调试版本构建
debug:
	@echo "构建调试版本..."
	./build/local-build.sh --debug

# 构建带 io_uring 支持的版本
build-io-uring:
	@echo "构建带 io_uring 支持的版本..."
	./build/local-build.sh --io-uring

# 调试版本带 io_uring
debug-io-uring:
	@echo "构建调试版本 (带 io_uring)..."
	./build/local-build.sh --debug --io-uring

# 运行测试
test:
	@echo "运行测试..."
	cargo test --release

# 检查依赖
check-deps:
	@echo "检查系统依赖..."
	@command -v cargo >/dev/null 2>&1 || { echo "❌ 错误: 未找到 cargo，请安装 Rust"; exit 1; }
	@command -v rustc >/dev/null 2>&1 || { echo "❌ 错误: 未找到 rustc，请安装 Rust"; exit 1; }
	@pkg-config --exists fuse3 2>/dev/null || pkg-config --exists fuse 2>/dev/null || { echo "❌ 错误: 未找到 FUSE 库，请安装 libfuse3-dev"; exit 1; }
	@echo "✅ 所有依赖已满足"

# 安装到系统
install: build
	@echo "安装 NFS-CacheFS..."
	@if [ ! -f "target/release/nfs-cachefs" ]; then \
		echo "错误: 未找到编译后的二进制文件，请先运行 'make build'"; \
		exit 1; \
	fi
	@echo "复制二进制文件到 /usr/local/bin..."
	sudo cp target/release/nfs-cachefs /usr/local/bin/
	sudo chmod 755 /usr/local/bin/nfs-cachefs
	@echo "创建 mount.cachefs 链接..."
	sudo ln -sf /usr/local/bin/nfs-cachefs /sbin/mount.cachefs
	@echo "✅ 安装完成"

# Docker 构建（备用方案）
docker-build:
	@echo "使用 Docker 构建（备用方案）..."
	./build/docker-build.sh

# 清理构建产物
clean:
	@echo "清理构建产物..."
	cargo clean
	rm -rf nfs-cachefs-v*-linux-x86_64/
	rm -f *.tar.gz *.tar.gz.sha256
	@echo "✅ 清理完成"

# 显示构建信息
info:
	@echo "项目信息:"
	@echo "  项目名称: nfs-cachefs"
	@echo "  构建方式: 本地 Rust 编译"
	@echo "  目标平台: Linux x86_64"
	@echo ""
	@echo "Rust 环境:"
	@command -v rustc >/dev/null 2>&1 && echo "  Rust 版本: $$(rustc --version)" || echo "  Rust: 未安装"
	@command -v cargo >/dev/null 2>&1 && echo "  Cargo 版本: $$(cargo --version)" || echo "  Cargo: 未安装"
	@echo ""
	@echo "系统依赖:"
	@pkg-config --exists fuse3 2>/dev/null && echo "  FUSE: fuse3 (已安装)" || \
		(pkg-config --exists fuse 2>/dev/null && echo "  FUSE: fuse (已安装)" || echo "  FUSE: 未安装")
	@echo ""
	@echo "构建产物:"
	@if [ -f "target/release/nfs-cachefs" ]; then \
		echo "  二进制文件: target/release/nfs-cachefs"; \
		ls -lh target/release/nfs-cachefs; \
	else \
		echo "  二进制文件: 未构建"; \
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