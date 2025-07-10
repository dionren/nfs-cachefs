# 创建 GitHub Release v0.4.0 指导

## 📋 准备就绪的文件

✅ 以下文件已经准备好上传：

1. **主发布包**: `nfs-cachefs-v0.4.0-linux-x86_64.tar.gz` (2.3MB)
2. **校验和文件**: `nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256`
3. **发布说明**: `release-notes-v0.4.0.md`

## 🚀 GitHub Release 创建步骤

### 方法一：通过 GitHub 网页界面 (推荐)

1. **访问 Releases 页面**
   ```
   https://github.com/dionren/nfs-cachefs/releases/new
   ```

2. **填写 Release 信息**
   - **Tag version**: `v0.4.0` (应该自动选择，因为我们已经推送了标签)
   - **Release title**: `NFS-CacheFS v0.4.0`
   - **Target**: `main` (默认)

3. **添加发布说明**
   复制 `release-notes-v0.4.0.md` 文件的内容到 "Describe this release" 文本框

4. **上传发布文件**
   拖拽或点击上传以下文件：
   - `nfs-cachefs-v0.4.0-linux-x86_64.tar.gz`
   - `nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256`

5. **发布设置**
   - ✅ 勾选 "Set as the latest release"
   - ⚠️ 不要勾选 "Set as a pre-release"

6. **点击 "Publish release"**

### 方法二：使用 GitHub CLI (需要认证)

如果你想使用命令行，可以运行：

```bash
# 首先认证 GitHub CLI
gh auth login

# 创建 Release
gh release create v0.4.0 \
  --title "NFS-CacheFS v0.4.0" \
  --notes-file release-notes-v0.4.0.md \
  nfs-cachefs-v0.4.0-linux-x86_64.tar.gz \
  nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256
```

## 📦 发布后验证

发布完成后，请验证：

1. **Release 页面**: https://github.com/dionren/nfs-cachefs/releases/tag/v0.4.0
2. **下载链接**:
   - https://github.com/dionren/nfs-cachefs/releases/download/v0.4.0/nfs-cachefs-v0.4.0-linux-x86_64.tar.gz
   - https://github.com/dionren/nfs-cachefs/releases/download/v0.4.0/nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256

3. **测试下载和校验**:
   ```bash
   wget https://github.com/dionren/nfs-cachefs/releases/download/v0.4.0/nfs-cachefs-v0.4.0-linux-x86_64.tar.gz
   wget https://github.com/dionren/nfs-cachefs/releases/download/v0.4.0/nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256
   sha256sum -c nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256
   ```

## 🎯 发布说明内容

以下是要复制到 GitHub Release 描述中的内容：

---

## 🚀 What's New

### Added
- **Automated Release Process**: Complete automated build and packaging system with binary compilation
- **GitHub Actions Integration**: Ready for automated releases through CI/CD
- **Comprehensive Build Scripts**: New `build-release.sh` script for optimized production builds
- **Enhanced Release Packaging**: All necessary files included in release packages

### Improved
- **Build Optimization**: Enhanced release builds with better optimization for production environments
- **Deployment Process**: Streamlined deployment with automated packaging and checksums
- **Documentation**: Updated changelog and release documentation

### Technical Details
- **Binary Size**: Optimized release binary (~2.3MB compressed)
- **Dependencies**: All dependencies properly packaged
- **Checksums**: SHA256 checksums included for integrity verification
- **Platform**: Linux x86_64 support

## 📦 Installation

### Quick Install
```bash
# Download and extract
wget https://github.com/dionren/nfs-cachefs/releases/download/v0.4.0/nfs-cachefs-v0.4.0-linux-x86_64.tar.gz
tar -xzf nfs-cachefs-v0.4.0-linux-x86_64.tar.gz
cd nfs-cachefs-v0.4.0-linux-x86_64

# Install
sudo ./install.sh
```

### Verify Checksum
```bash
# Verify download integrity
sha256sum -c nfs-cachefs-v0.4.0-linux-x86_64.tar.gz.sha256
```

## 🔧 Usage

```bash
# Basic usage
nfs-cachefs /path/to/nfs/share /path/to/mountpoint

# With custom cache settings
nfs-cachefs /nfs/share /mnt/cache --cache-size 20 --cache-dir /tmp/nfs-cache
```

## 📋 What's Included

- `nfs-cachefs` - Main binary executable
- `install.sh` - Installation script
- `mount.cachefs` - Mount helper
- `README.md` - Documentation
- `LICENSE` - License information
- `CHANGELOG.md` - Version history
- `docs/` - Additional documentation

## 🧪 Testing

All tests pass successfully:
- ✅ 24 library tests
- ✅ 2 integration tests
- ✅ Doc tests

## 🔍 System Requirements

- Linux x86_64
- FUSE3 or FUSE2 development libraries
- Sufficient disk space for cache

## 🐛 Known Issues

None reported for this release.

## 🤝 Contributing

We welcome contributions! Please see our documentation for development setup and contribution guidelines.

## 📄 License

This project is licensed under the terms included in the LICENSE file.

---

**Full Changelog**: https://github.com/dionren/nfs-cachefs/compare/v0.3.0...v0.4.0

---

## ✅ 完成检查清单

- [ ] 访问 GitHub Releases 页面
- [ ] 选择 v0.4.0 标签
- [ ] 填写 Release 标题
- [ ] 复制发布说明
- [ ] 上传 tar.gz 文件
- [ ] 上传 sha256 文件
- [ ] 设置为最新版本
- [ ] 点击发布
- [ ] 验证下载链接工作正常

🎉 发布完成后，v0.4.0 版本就可以供用户下载使用了！