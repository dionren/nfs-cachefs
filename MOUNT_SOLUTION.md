# NFS-CacheFS 挂载解决方案

## 问题总结

你遇到的不是bug，而是正常行为。NFS-CacheFS 在挂载时看起来"卡住"是因为：

1. **FUSE 文件系统的工作原理**：`fuser::mount2` 是一个阻塞调用，会持续运行直到文件系统被卸载
2. **前台运行**：程序默认在前台运行，占用终端
3. **mount 命令的期望**：`mount` 命令期望 mount helper 在成功挂载后立即退出

## 解决方案

### 方案 1：使用 `&` 后台运行（立即可用）

```bash
# 直接在命令后加 & 让其在后台运行
mount -t cachefs cachefs /mnt/chenyu-nfs \
    -o nfs_backend=/mnt/chenyu-nvme,cache_dir=/mnt/nvme/cachefs,cache_size_gb=100,allow_other &
```

### 方案 2：使用 nohup（立即可用）

```bash
# 使用 nohup 让进程脱离终端
nohup mount -t cachefs cachefs /mnt/chenyu-nfs \
    -o nfs_backend=/mnt/chenyu-nvme,cache_dir=/mnt/nvme/cachefs,cache_size_gb=100,allow_other \
    > /var/log/nfs-cachefs.log 2>&1 &
```

### 方案 3：使用 systemd 服务（推荐用于生产环境）

创建 `/etc/systemd/system/nfs-cachefs.service`：

```ini
[Unit]
Description=NFS-CacheFS Mount Service
After=network.target

[Service]
Type=forking
ExecStart=/usr/local/bin/nfs-cachefs /mnt/chenyu-nvme /mnt/chenyu-nfs \
    --cache-dir /mnt/nvme/cachefs \
    --cache-size 100
ExecStop=/bin/fusermount3 -u /mnt/chenyu-nfs
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

然后启动服务：

```bash
sudo systemctl daemon-reload
sudo systemctl enable nfs-cachefs
sudo systemctl start nfs-cachefs
```

### 方案 4：修改代码支持自动后台运行（已实现）

我已经修改了代码，添加了自动后台运行功能。重新编译后，mount 命令会自动在后台运行：

```bash
# 重新编译
cd /workspace
cargo build --release

# 安装新版本
sudo cp target/release/nfs-cachefs /usr/local/bin/
sudo ln -sf /usr/local/bin/nfs-cachefs /sbin/mount.cachefs

# 现在 mount 会自动后台运行
sudo mount -t cachefs cachefs /mnt/chenyu-nfs \
    -o nfs_backend=/mnt/chenyu-nvme,cache_dir=/mnt/nvme/cachefs,cache_size_gb=100,allow_other

# 如果需要前台运行（用于调试）
sudo mount -t cachefs cachefs /mnt/chenyu-nfs \
    -o nfs_backend=/mnt/chenyu-nvme,cache_dir=/mnt/nvme/cachefs,cache_size_gb=100,allow_other,foreground
```

## 验证挂载

```bash
# 检查挂载状态
mount | grep chenyu-nfs

# 检查进程
ps aux | grep nfs-cachefs

# 测试文件系统
ls -la /mnt/chenyu-nfs

# 查看日志（如果使用 nohup）
tail -f /var/log/nfs-cachefs.log
```

## 卸载文件系统

```bash
# 方法 1：使用 umount
sudo umount /mnt/chenyu-nfs

# 方法 2：使用 fusermount3
sudo fusermount3 -u /mnt/chenyu-nfs

# 方法 3：如果使用 systemd
sudo systemctl stop nfs-cachefs
```

## 故障排除

### 1. 检查挂载是否成功

```bash
# 应该看到 cachefs 类型的挂载
mount | grep cachefs

# 应该返回 0（表示是挂载点）
mountpoint /mnt/chenyu-nfs; echo $?
```

### 2. 如果挂载失败

```bash
# 查看系统日志
sudo journalctl -xe | grep cachefs

# 检查 dmesg
dmesg | tail -20
```

### 3. 权限问题

确保使用了 `allow_other` 选项，否则只有 root 用户可以访问挂载点。

## 性能监控

挂载成功后，可以监控缓存性能：

```bash
# 查看缓存目录大小
du -sh /mnt/nvme/cachefs

# 监控 I/O
iostat -x 1

# 查看文件系统活动
watch -n 1 'ls -la /mnt/chenyu-nfs | head -10'
```

## 总结

NFS-CacheFS 的"卡住"现象是正常的 FUSE 文件系统行为。通过以上任一方案都可以解决这个问题，推荐：

- **开发/测试环境**：使用方案 1 或 2（简单快速）
- **生产环境**：使用方案 3（systemd 服务，更可靠）
- **长期解决**：使用方案 4（代码已修改，重新编译即可）