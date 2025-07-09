# NFS-CacheFS v0.1.0 重新发布清单

## 发布信息

- **版本**: v0.1.0
- **发布日期**: 2024-07-09
- **重新发布原因**: 确保二进制文件完整性
- **发布包**: nfs-cachefs-v0.1.0-linux-x86_64.tar.gz

## 文件清单

### 发布包内容
```
v0.1.0/
├── nfs-cachefs          # 主二进制文件 (2.2MB)
├── install.sh           # 安装脚本
├── INSTALL.md          # 安装说明
├── RELEASE_NOTES.md    # 发布说明
├── VERSION             # 版本信息
└── SHA256SUMS          # 校验和文件
```

### 校验和信息

#### 发布包校验和
```
c4cefc14af181870c68fdbdca44d62df3930a343f5004e3f3db8469113e85223  nfs-cachefs-v0.1.0-linux-x86_64.tar.gz
```

#### 二进制文件校验和
```
2295ffbeea26cea0a95c8c0b1cd25738d3964a68ec7622ce20d81d06588b4dfc  nfs-cachefs
```

## 系统要求

- **操作系统**: Ubuntu 22.04 LTS / Ubuntu 24.04 LTS
- **架构**: x86_64 (64-bit)
- **内核**: Linux 5.4+
- **依赖**: libfuse3-3, fuse3

## 安装说明

### 快速安装
```bash
# 下载发布包
wget https://github.com/your-org/nfs-cachefs/releases/download/v0.1.0/nfs-cachefs-v0.1.0-linux-x86_64.tar.gz

# 验证校验和
echo "c4cefc14af181870c68fdbdca44d62df3930a343f5004e3f3db8469113e85223  nfs-cachefs-v0.1.0-linux-x86_64.tar.gz" | sha256sum -c

# 解压并安装
tar -xzf nfs-cachefs-v0.1.0-linux-x86_64.tar.gz
cd v0.1.0
sudo ./install.sh
```

### 验证安装
```bash
# 检查版本
nfs-cachefs --version

# 查看帮助
nfs-cachefs --help
```

## 功能特性

- ⚡ 零延迟首次访问 - 异步缓存填充
- 🚀 透明加速 - 对应用程序完全透明
- 💾 智能缓存管理 - 自动LRU驱逐
- 🔒 数据完整性 - 原子操作确保缓存文件完整
- 📊 实时监控 - 内置性能指标
- 🔐 只读模式 - 专为只读工作负载优化

## 已知问题

- 二进制文件依赖 libfuse3.so.3，需要目标系统安装 FUSE 3.0+
- 仅支持只读模式，不支持写操作
- 需要 Linux 内核 5.4+ 支持

## 技术支持

如有问题，请参考：
- INSTALL.md - 详细安装说明
- RELEASE_NOTES.md - 发布说明
- README.md - 项目文档