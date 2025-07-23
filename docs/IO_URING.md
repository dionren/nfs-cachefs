# io_uring 集成指南

## 概述

NFS-CacheFS 现已支持 Linux io_uring，这是一个高性能异步 I/O 框架，可显著提升文件系统性能。

## 性能提升

启用 io_uring 后，预期性能提升：

- **缓存命中**: 20-30倍提升 (100 MB/s → 2-3 GB/s)
- **缓存未命中**: 3-8倍提升 (60 MB/s → 200-500 MB/s)
- **CPU使用率**: 降低 50-70%
- **系统调用**: 减少 80%

## 系统要求

- Linux 内核 5.10+ (基础支持)
- Linux 内核 5.11+ (splice 支持)
- Linux 内核 5.19+ (固定缓冲区支持)

## 构建

### 使用 Make

```bash
# 构建带 io_uring 的发布版本
make build-io-uring

# 构建带 io_uring 的调试版本
make debug-io-uring
```

### 使用 Cargo

```bash
# 构建
cargo build --release --features io_uring

# 测试
cargo test --features io_uring
```

## 配置

在挂载时启用 io_uring：

```bash
# 使用 mount 命令
sudo mount -t cachefs -o \
  nfs_backend=/mnt/nfs,\
  cache_dir=/mnt/nvme/cache,\
  cache_size_gb=100,\
  nvme_use_io_uring=true,\
  nvme_queue_depth=256,\
  nvme_polling_mode=true \
  cachefs /mnt/cached

# 或直接运行
sudo nfs-cachefs /mnt/nfs /mnt/cached -o \
  cache_dir=/mnt/nvme/cache,\
  nvme_use_io_uring=true
```

## 配置选项

| 选项 | 默认值 | 说明 |
|-----|--------|------|
| `nvme_use_io_uring` | false | 启用 io_uring |
| `nvme_queue_depth` | 128 | io_uring 队列深度 |
| `nvme_polling_mode` | false | 启用 SQ 轮询模式 |
| `nvme_io_poll` | false | 启用 I/O 轮询 |
| `nvme_fixed_buffers` | true | 使用固定缓冲区 |
| `nvme_use_hugepages` | false | 使用大页内存 |
| `nvme_sq_poll_idle_ms` | 1000 | SQ 轮询空闲时间 |

## 工作原理

### 1. 缓存读取优化

当文件在缓存中时，使用 io_uring 进行零拷贝读取：

```
应用程序 → FUSE → io_uring → NVMe → 应用程序
```

传统方式需要多次内存拷贝，io_uring 直接在内核中完成数据传输。

### 2. 缓存写入优化

对于大文件（>10MB），使用 splice 进行零拷贝传输：

```
NFS → splice → 缓存文件
```

避免了数据在用户空间和内核空间之间的拷贝。

### 3. 批量 I/O

io_uring 支持批量提交和完成，减少系统调用开销。

## 性能调优

### 1. 队列深度

增加队列深度可提高并发性能：

```bash
nvme_queue_depth=512
```

### 2. 轮询模式

对于低延迟场景，启用轮询模式：

```bash
nvme_polling_mode=true
nvme_io_poll=true
```

### 3. CPU 亲和性

绑定到特定 CPU 核心：

```bash
taskset -c 0-3 nfs-cachefs ...
```

### 4. 内存优化

使用大页内存：

```bash
# 配置系统
echo 1024 > /proc/sys/vm/nr_hugepages

# 启用大页
nvme_use_hugepages=true
```

## 监控

查看 io_uring 性能指标：

```bash
# 查看日志中的性能信息
grep "io_uring" /var/log/syslog

# 关键指标
- 提交次数
- 完成次数
- 平均延迟
- P99 延迟
```

## 故障排除

### 1. 检查内核支持

```bash
# 检查内核版本
uname -r

# 检查 io_uring 支持
grep io_uring /proc/kallsyms
```

### 2. 降级到传统 I/O

如果遇到问题，可禁用 io_uring：

```bash
nvme_use_io_uring=false
```

### 3. 调试模式

启用详细日志：

```bash
RUST_LOG=nfs_cachefs=debug nfs-cachefs ...
```

## 基准测试

使用 fio 测试性能：

```bash
# 测试读取性能
fio --name=read \
    --ioengine=io_uring \
    --direct=1 \
    --rw=read \
    --bs=4M \
    --size=10G \
    --numjobs=4
```

## 注意事项

1. io_uring 需要 root 权限
2. 某些容器环境可能不支持
3. 确保有足够的内存用于缓冲区
4. 监控系统资源使用情况

## 相关链接

- [io_uring 官方文档](https://kernel.dk/io_uring.pdf)
- [Linux 内核文档](https://www.kernel.org/doc/html/latest/filesystems/io_uring.html)
- [性能优化指南](https://github.com/axboe/liburing/wiki/Performance)