# 构建指南

## 快速开始

使用本地 Rust 环境构建：

```bash
make build
```

## 前置要求

### 1. 安装 Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 2. 安装系统依赖

**Ubuntu/Debian:**
```bash
sudo apt-get update
sudo apt-get install libfuse3-dev pkg-config build-essential
```

**CentOS/RHEL:**
```bash
sudo yum install fuse3-devel pkgconfig gcc
```

**Fedora:**
```bash
sudo dnf install fuse3-devel pkgconfig gcc
```

**Arch Linux:**
```bash
sudo pacman -S fuse3 pkgconf base-devel
```

## 构建选项

### 发布版本（优化）
```bash
make build
# 或
make release
```

### 调试版本
```bash
make debug
```

### 运行测试
```bash
make test
```

### 安装到系统
```bash
make install
```

### 清理构建
```bash
make clean
```

### 检查依赖
```bash
make check-deps
```

## 手动构建

如果不使用 Makefile，可以直接使用 cargo：

```bash
# 发布版本
cargo build --release

# 调试版本  
cargo build

# 运行测试
cargo test
```

## 构建产物

构建完成后，二进制文件位于：
- 发布版本: `target/release/nfs-cachefs`
- 调试版本: `target/debug/nfs-cachefs`

## Docker 构建（备用）

如果本地环境有问题，仍可使用 Docker 构建：

```bash
make docker-build
```

## 故障排除

### 1. 找不到 FUSE 库
确保安装了 `libfuse3-dev` 或 `libfuse-dev`

### 2. 编译错误
更新 Rust 到最新版本：
```bash
rustup update
```

### 3. 链接错误
确保安装了所有系统依赖，运行：
```bash
make check-deps
```