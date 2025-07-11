# Changelog
## [0.5.2] - 2025-07-11

### Added
- 完整的自动化发布流程和脚本
- 详细的发布流程文档 (RELEASE_PROCESS.md)
- 自动化版本号更新功能

### Changed
- 改进 Docker 构建系统的稳定性
- 优化 release.sh 脚本的错误处理
- 统一发布包命名和版本管理

### Fixed
- 修复发布脚本中的版本号同步问题
- 改进构建产物的清理和验证流程
- 优化发布流程中的依赖检查

## [0.5.1] - 2025-01-10

### Fixed
- **重大性能修复**：解决缓存未命中时NFS读取极慢的问题
- 重构NFS读取逻辑，避免异步执行器阻塞
- 缓存未命中时直接使用同步读取，保持NFS客户端优化
- 异步缓存任务不再阻塞前台读取操作

### Changed
- FUSE读取逻辑优化：缓存命中走异步，缓存未命中走同步
- 简化异步执行器，仅处理缓存文件读取
- 后台缓存任务完全异步化，不影响用户读取性能

### Technical Details
- 移除了NFS读取的异步队列瓶颈
- 保持了NFS客户端的预读、缓存等优化机制
- 大幅提升了首次文件访问的响应速度

## [0.5.0] - 2025-07-10

### Added
- 重构构建系统为 Docker 方式
- 添加完整的发布自动化流程
- 新增 GitHub Actions 自动发布工作流

### Changed
- 统一使用 Docker 构建，移除本地构建依赖
- 重新组织 build 目录结构
- 更新 Makefile 支持 Docker 构建

### Fixed
- 修复构建环境依赖问题
- 优化发布流程和文档

## [0.4.1] - 2025-07-10

### Added
- 重构构建系统为 Docker 方式
- 添加完整的发布自动化流程
- 新增 GitHub Actions 自动发布工作流

### Changed
- 统一使用 Docker 构建，移除本地构建依赖
- 重新组织 build 目录结构
- 更新 Makefile 支持 Docker 构建

### Fixed
- 修复构建环境依赖问题
- 优化发布流程和文档


All notable changes to this project will be documented in this file.

## [0.4.0] - 2025-01-10

### Added
- Automated release process with binary compilation
- GitHub Actions workflow for automated releases
- Comprehensive build and packaging scripts

### Improved
- Enhanced release packaging with all necessary files
- Better build optimization for production releases
- Streamlined deployment process

## [0.3.0] - 2025-01-10

### Added
- Automatic daemonization support for mount helper mode
- Background running by default when invoked via mount command
- `foreground` mount option to disable automatic daemonization
- Comprehensive mount troubleshooting documentation

### Fixed
- Mount command hanging issue - now properly forks to background
- FUSE mount options handling for better compatibility
- Signal handling for graceful shutdown

### Changed
- Mount helper now runs in background by default
- Improved logging with thread IDs for better debugging
- Enhanced error messages for mount failures

### Documentation
- Added `MOUNT_SOLUTION.md` with detailed mounting instructions
- Added `NFS_CACHEFS_TROUBLESHOOTING.md` for common issues
- Updated README with clearer mount examples

## [0.2.0] - 2024-12-10

### Added
- Read-only filesystem mode (removed write support)
- Improved cache management with LRU eviction
- Better error handling and recovery

### Changed
- Filesystem is now read-only by default
- File permissions set to 0o444 (read-only)
- Directory permissions set to 0o555

## [0.1.0] - 2024-11-15

### Added
- Initial release
- Basic NFS caching functionality
- Asynchronous cache filling
- FUSE filesystem implementation