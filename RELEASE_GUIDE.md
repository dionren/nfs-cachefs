# NFS-CacheFS 发布指南

## 发布流程

### 1. 准备发布

1. **更新版本号**
   - 编辑 `Cargo.toml` 中的 `version`
   - 更新 `src/main.rs` 中的版本字符串（两处）
   - 更新 `README.md` 中的版本徽章和下载链接

2. **更新文档**
   - 更新 `CHANGELOG.md` 添加新版本的更改
   - 创建 `RELEASE_NOTES_vX.X.X.md` 文件
   - 更新 `README.md` 中的最新版本说明

3. **测试构建**
   ```bash
   cargo test
   cargo build --release
   ```

### 2. 本地构建发布包

```bash
# 运行构建脚本
./build-release.sh

# 验证生成的文件
ls -la nfs-cachefs-v*.tar.gz*
sha256sum -c nfs-cachefs-v*.tar.gz.sha256
```

### 3. Git 操作

```bash
# 提交所有更改
git add .
git commit -m "Release v0.3.0"

# 创建标签
git tag -a v0.3.0 -m "Release version 0.3.0"

# 推送到远程仓库
git push origin main
git push origin v0.3.0
```

### 4. GitHub Release

有两种方式创建 release：

#### 方式 1：自动（推荐）
如果已配置 GitHub Actions，推送标签后会自动创建 release 并上传二进制文件。

#### 方式 2：手动
1. 访问 https://github.com/yourusername/nfs-cachefs/releases/new
2. 选择刚创建的标签 `v0.3.0`
3. 填写 Release 标题：`NFS-CacheFS v0.3.0`
4. 复制 `RELEASE_NOTES_v0.3.0.md` 的内容到描述
5. 上传文件：
   - `nfs-cachefs-v0.3.0-linux-x86_64.tar.gz`
   - `nfs-cachefs-v0.3.0-linux-x86_64.tar.gz.sha256`
6. 发布 Release

### 5. 清理

发布完成后，删除本地的二进制文件：

```bash
rm -f nfs-cachefs-v*.tar.gz*
rm -rf build/
```

## 版本号规范

遵循语义化版本控制：

- **主版本号**：不兼容的 API 更改
- **次版本号**：向后兼容的功能添加
- **修订号**：向后兼容的问题修复

示例：
- `v1.0.0` - 首个稳定版本
- `v1.1.0` - 添加新功能
- `v1.1.1` - 修复 bug

## 检查清单

发布前确保：

- [ ] 所有测试通过
- [ ] 版本号已更新（Cargo.toml, main.rs）
- [ ] CHANGELOG.md 已更新
- [ ] RELEASE_NOTES_vX.X.X.md 已创建
- [ ] README.md 已更新
- [ ] 代码已提交并推送
- [ ] 标签已创建并推送
- [ ] GitHub Release 已创建
- [ ] 二进制文件已上传到 Release
- [ ] 下载链接可用

## 注意事项

1. **不要将二进制文件提交到 Git**
   - 二进制文件应该上传到 GitHub Releases
   - `.gitignore` 已配置忽略发布文件

2. **保持版本一致性**
   - 确保所有地方的版本号都已更新
   - 使用搜索功能查找所有版本引用

3. **测试下载链接**
   - 发布后测试 README 中的下载链接
   - 验证 sha256 校验和

4. **更新依赖**
   - 定期更新 Rust 依赖
   - 在主要版本发布前运行 `cargo update`