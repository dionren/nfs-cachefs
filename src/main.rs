// use std::env;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process;
use std::env;
use std::sync::Arc;

use clap::{Arg, Command};
use fuser::MountOption;
use tracing::{error, info, warn};
use tracing_subscriber;

use nfs_cachefs::core::config::Config;
use nfs_cachefs::fs::cachefs::CacheFs;

mod mount_helper;

/// 检查是否以 mount helper 模式运行
fn is_mount_helper_mode() -> bool {
    if let Some(program_name) = env::args().next() {
        program_name.ends_with("mount.cachefs") || 
        (env::args().count() >= 4 && env::args().any(|arg| arg == "-o"))
    } else {
        false
    }
}

/// 解析 mount helper 参数
fn parse_mount_helper_args() -> Result<(Config, PathBuf, Vec<MountOption>), String> {
    let args: Vec<String> = env::args().collect();
    
    // mount.cachefs <source> <target> -o <options>
    if args.len() < 4 {
        return Err("Invalid mount helper arguments".to_string());
    }
    
    let _source = &args[1];  // 忽略source，我们使用选项中的nfs_backend
    let mountpoint = PathBuf::from(&args[2]);
    
    let mut mount_options = Vec::new();
    let mut config_options = HashMap::new();
    let mut should_daemonize = true;  // 默认后台运行
    
    // 强制只读模式
    mount_options.push(MountOption::RO);
    
    // 解析 -o 选项
    let mut i = 3;
    while i < args.len() {
        if args[i] == "-o" && i + 1 < args.len() {
            let options_str = &args[i + 1];
            for option in options_str.split(',') {
                let option = option.trim();
                if option.is_empty() {
                    continue;
                }
                
                // 解析 key=value 格式的选项
                if let Some((key, value)) = option.split_once('=') {
                    config_options.insert(key.to_string(), value.to_string());
                } else {
                    // 处理标志选项
                    match option {
                        "ro" => {
                            // 已经默认设置为只读，忽略
                        }
                        "rw" => {
                            warn!("Read-write mode is not supported, filesystem will be mounted read-only");
                            // 不添加 RW 选项，保持只读
                        }
                        "allow_other" => {
                            mount_options.push(MountOption::AllowOther);
                        }
                        "allow_root" => {
                            mount_options.push(MountOption::AllowRoot);
                        }
                        "auto_unmount" => {
                            mount_options.push(MountOption::AutoUnmount);
                        }
                        "foreground" | "fg" => {
                            should_daemonize = false;
                        }
                        _ => {
                            // 未知选项，作为自定义选项处理
                            mount_options.push(MountOption::CUSTOM(option.to_string()));
                        }
                    }
                }
            }
            break;
        }
        i += 1;
    }
    
    // 如果需要后台运行，添加到配置中
    if should_daemonize {
        config_options.insert("_daemonize".to_string(), "true".to_string());
    }
    
    // 从配置选项创建Config
    let nfs_backend = config_options.get("nfs_backend")
        .ok_or("Missing required option: nfs_backend")?;
    let nfs_backend_path = PathBuf::from(nfs_backend);
    
    let cache_dir = config_options.get("cache_dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/nfs-cachefs"));
    
    let cache_size_gb = config_options.get("cache_size_gb")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    
    let block_size_mb = config_options.get("block_size_mb")
        .and_then(|s| s.parse().ok())
        .unwrap_or(64);
    
    let max_concurrent_caching = config_options.get("max_concurrent")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    
    let min_cache_file_size_mb = config_options.get("min_cache_file_size_mb")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    
    // 📊 FUSE性能优化参数 - mount helper模式
    let max_read_mb = block_size_mb.min(16); // 降低到16MB以提高兼容性
    
    // 添加兼容的FUSE性能优化挂载选项
    mount_options.push(MountOption::CUSTOM(format!("max_read={}", max_read_mb * 1024 * 1024)));
    // 注意：某些FUSE选项可能不被所有版本支持，只添加兼容的选项
    
    // 设置预读大小以匹配块大小
    let readahead_bytes = max_read_mb * 2 * 1024 * 1024; // 预读为max_read的2倍
    
    let config = Config {
        nfs_backend_path,
        cache_dir,
        mount_point: mountpoint.clone(),
        max_cache_size_bytes: cache_size_gb * 1024 * 1024 * 1024,
        cache_block_size: block_size_mb * 1024 * 1024,
        max_concurrent_caching,
        enable_checksums: true,
        cache_ttl_seconds: None,
        eviction_policy: nfs_cachefs::core::config::EvictionPolicy::Lru,
        direct_io: true,
        readahead_bytes: readahead_bytes,  // 使用计算的预读大小
        min_cache_file_size: min_cache_file_size_mb * 1024 * 1024,
        allow_async_read: false, // 使用同步直读获得更好的性能
        smart_cache: nfs_cachefs::core::config::SmartCacheConfig::default(),
        nvme: nfs_cachefs::core::config::NvmeConfig::default(),
    };
    
    Ok((config, mountpoint, mount_options))
}

/// 解析命令行参数
fn parse_args() -> (Config, PathBuf, Vec<MountOption>) {
    // 检查是否以mount helper模式运行
    if is_mount_helper_mode() {
        match parse_mount_helper_args() {
            Ok(result) => return result,
            Err(e) => {
                error!("Mount helper mode error: {}", e);
                process::exit(1);
            }
        }
    }
    
    // 原有的命令行参数解析逻辑
    let matches = Command::new("nfs-cachefs")
        .version("0.6.0")
        .author("NFS-CacheFS Team")
        .about("High-performance read-only asynchronous caching filesystem for NFS")
        .arg(
            Arg::new("nfs_backend")
                .help("NFS backend directory path")
                .required(true)
                .index(1),
        )
        .arg(
            Arg::new("mountpoint")
                .help("Mount point directory")
                .required(true)
                .index(2),
        )
        .arg(
            Arg::new("options")
                .short('o')
                .long("options")
                .help("Mount options (comma-separated)")
                .value_name("OPTIONS")
                .action(clap::ArgAction::Set),
        )
        .arg(
            Arg::new("cache_dir")
                .long("cache-dir")
                .help("Cache directory path")
                .value_name("PATH")
                .action(clap::ArgAction::Set),
        )
        .arg(
            Arg::new("cache_size")
                .long("cache-size")
                .help("Cache size in GB")
                .value_name("SIZE")
                .action(clap::ArgAction::Set),
        )
        .arg(
            Arg::new("block_size")
                .long("block-size")
                .help("Block size in MB")
                .value_name("SIZE")
                .action(clap::ArgAction::Set),
        )
        .arg(
            Arg::new("max_concurrent_tasks")
                .long("max-concurrent-tasks")
                .help("Maximum concurrent caching tasks")
                .value_name("COUNT")
                .action(clap::ArgAction::Set),
        )
        .arg(
            Arg::new("foreground")
                .short('f')
                .long("foreground")
                .help("Run in foreground")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("debug")
                .short('d')
                .long("debug")
                .help("Enable debug logging")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("min_cache_file_size")
                .long("min-cache-file-size")
                .help("Minimum file size to cache in MB (default: 100)")
                .value_name("SIZE_MB")
                .action(clap::ArgAction::Set),
        )
        .get_matches();

    let nfs_backend = PathBuf::from(matches.get_one::<String>("nfs_backend").unwrap());
    let mountpoint = PathBuf::from(matches.get_one::<String>("mountpoint").unwrap());
    
    // 解析挂载选项
    let mut mount_options = Vec::new();
    let mut config_options = HashMap::new();
    
    // 强制只读模式
    mount_options.push(MountOption::RO);
    
    if let Some(options_str) = matches.get_one::<String>("options") {
        for option in options_str.split(',') {
            let option = option.trim();
            if option.is_empty() {
                continue;
            }
            
            // 解析 key=value 格式的选项
            if let Some((key, value)) = option.split_once('=') {
                config_options.insert(key.to_string(), value.to_string());
            } else {
                // 处理标志选项
                match option {
                    "ro" => {
                        // 已经默认设置为只读，忽略
                    }
                    "rw" => {
                        warn!("Read-write mode is not supported, filesystem will be mounted read-only");
                        // 不添加 RW 选项，保持只读
                    }
                    "allow_other" => mount_options.push(MountOption::AllowOther),
                    "allow_root" => mount_options.push(MountOption::AllowRoot),
                    "auto_unmount" => mount_options.push(MountOption::AutoUnmount),
                    "foreground" | "fg" => {
                        // foreground 选项不应该传递给 FUSE，由程序自己处理
                        // 这里不做任何操作，因为前台运行逻辑已经在 main 函数中处理了
                    },
                    _ => {
                        // 未知选项，作为自定义选项处理
                        mount_options.push(MountOption::CUSTOM(option.to_string()));
                    }
                }
            }
        }
    }
    
    // 从命令行参数创建配置（简化实现）
    let cache_dir = matches
        .get_one::<String>("cache_dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/nfs-cachefs"));
    
    let cache_size_gb = matches
        .get_one::<String>("cache_size")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    
    let block_size_mb = matches
        .get_one::<String>("block_size")
        .and_then(|s| s.parse().ok())
        .unwrap_or(64);
    
    let max_concurrent_caching = matches
        .get_one::<String>("max_concurrent_tasks")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    
    let min_cache_file_size_mb = matches
        .get_one::<String>("min_cache_file_size")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    
    // 📊 FUSE性能优化参数
    let max_read_mb = block_size_mb.min(16); // 降低到16MB以提高兼容性
    
    // 添加兼容的FUSE性能优化挂载选项
    mount_options.push(MountOption::CUSTOM(format!("max_read={}", max_read_mb * 1024 * 1024)));
    // 注意：某些FUSE选项可能不被所有版本支持，只添加兼容的选项
    
    // 设置预读大小以匹配块大小
    let readahead_bytes = max_read_mb * 2 * 1024 * 1024; // 预读为max_read的2倍
    
    let config = Config {
        nfs_backend_path: nfs_backend.clone(),
        cache_dir,
        mount_point: mountpoint.clone(),
        max_cache_size_bytes: cache_size_gb * 1024 * 1024 * 1024,
        cache_block_size: block_size_mb * 1024 * 1024,
        max_concurrent_caching,
        enable_checksums: true,
        cache_ttl_seconds: None,
        eviction_policy: nfs_cachefs::core::config::EvictionPolicy::Lru,
        direct_io: true,
        readahead_bytes: readahead_bytes,  // 使用计算的预读大小
        min_cache_file_size: min_cache_file_size_mb * 1024 * 1024,
        allow_async_read: false, // 使用同步直读获得更好的性能
        smart_cache: nfs_cachefs::core::config::SmartCacheConfig::default(),
        nvme: nfs_cachefs::core::config::NvmeConfig::default(),
    };
    
    // 不要将 foreground 传递给 FUSE，程序会自己处理前台运行
    // if matches.get_flag("foreground") {
    //     mount_options.push(MountOption::CUSTOM("foreground".to_string()));
    // }
    
    (config, mountpoint, mount_options)
}

/// 初始化日志系统
fn init_logging(log_level: &str) {
    let level = match log_level.to_lowercase().as_str() {
        "trace" => tracing::Level::TRACE,
        "debug" => tracing::Level::DEBUG,
        "info" => tracing::Level::INFO,
        "warn" => tracing::Level::WARN,
        "error" => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    };
    
    // 创建自定义的日志格式，突出显示缓存和性能相关的日志
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .with_thread_ids(false)  // 关闭线程ID以减少干扰
        .with_line_number(false)  // 关闭行号以保持日志简洁
        .with_level(true)
        .with_ansi(true)  // 启用彩色输出
        .compact()  // 使用紧凑格式
        .init();
}

/// 初始化详细日志系统（用于调试和性能分析）
fn init_verbose_logging() {
    // 为缓存和性能分析启用详细日志
    std::env::set_var("RUST_LOG", "nfs_cachefs=info,warn");
    
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .with_thread_ids(false)
        .with_line_number(false)
        .with_level(true)
        .with_ansi(true)
        .compact()
        .init();
        
    // 打印性能监控提示
    tracing::info!("🔍 PERFORMANCE MONITORING ENABLED");
    tracing::info!("📊 Cache operations will be logged with detailed timing");
    tracing::info!("🚀 Look for emoji indicators: 📁=read, 🚀=cache hit, ❌=cache miss, 🔄=caching, ✅=success");
}

/// 验证配置
fn validate_config(config: &Config) -> Result<(), String> {
    // 检查 NFS 后端目录是否存在
    if !config.nfs_backend_path.exists() {
        return Err(format!("NFS backend directory does not exist: {}", config.nfs_backend_path.display()));
    }
    
    if !config.nfs_backend_path.is_dir() {
        return Err(format!("NFS backend path is not a directory: {}", config.nfs_backend_path.display()));
    }
    
    // 检查缓存目录
    if let Some(parent) = config.cache_dir.parent() {
        if !parent.exists() {
            return Err(format!("Cache directory parent does not exist: {}", parent.display()));
        }
    }
    
    // 创建缓存目录（如果不存在）
    if !config.cache_dir.exists() {
        std::fs::create_dir_all(&config.cache_dir)
            .map_err(|e| format!("Failed to create cache directory: {}", e))?;
    }
    
    // 检查缓存大小
    if config.max_cache_size_bytes == 0 {
        return Err("Cache size must be greater than 0".to_string());
    }
    
    if config.max_cache_size_bytes > 1000 * 1024 * 1024 * 1024 {
        warn!("Cache size is very large ({}GB), make sure you have enough disk space", config.max_cache_size_bytes / (1024 * 1024 * 1024));
    }
    
    // 检查块大小
    if config.cache_block_size == 0 || config.cache_block_size > 1024 * 1024 * 1024 {
        return Err("Block size must be between 1 and 1024 MB".to_string());
    }
    
    // 检查并发任务数
    if config.max_concurrent_caching == 0 {
        return Err("Max concurrent tasks must be greater than 0".to_string());
    }
    
    if config.max_concurrent_caching > 100 {
        warn!("Very high concurrent tasks count ({}), this may impact performance", config.max_concurrent_caching);
    }
    
    Ok(())
}

/// 检查挂载点
fn validate_mountpoint(mountpoint: &PathBuf) -> Result<(), String> {
    if !mountpoint.exists() {
        return Err(format!("Mount point does not exist: {}", mountpoint.display()));
    }
    
    if !mountpoint.is_dir() {
        return Err(format!("Mount point is not a directory: {}", mountpoint.display()));
    }
    
    // 检查挂载点是否为空
    match std::fs::read_dir(mountpoint) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                warn!("Mount point is not empty: {}", mountpoint.display());
            }
        }
        Err(e) => {
            return Err(format!("Cannot read mount point directory: {}", e));
        }
    }
    
    Ok(())
}

/// 主函数
#[tokio::main]
async fn main() {
    // 先解析命令行参数以确定是否需要前台运行
    let args: Vec<String> = std::env::args().collect();
    let is_foreground = args.iter().any(|arg| arg == "--foreground" || arg == "-f") || 
                       args.iter().any(|arg| arg.contains("foreground"));
    
    // 检查是否需要后台运行（在解析参数之前）
    if mount_helper::should_daemonize(&args) && !is_foreground {
        // 在日志初始化之前进行守护进程化
        if let Err(e) = mount_helper::daemonize() {
            eprintln!("Failed to daemonize: {}", e);
            process::exit(1);
        }
    }
    
    // 解析命令行参数
    let (config, mountpoint, mount_options) = parse_args();
    
    // 初始化详细日志系统以便观察缓存性能
    init_verbose_logging();
    
    info!("🚀 Starting NFS-CacheFS v0.6.0 (READ-ONLY MODE)");
    info!("📁 NFS Backend: {}", config.nfs_backend_path.display());
    info!("💾 Cache Directory: {}", config.cache_dir.display());
    info!("📍 Mount Point: {}", mountpoint.display());
    info!("💿 Cache Size: {}GB", config.max_cache_size_bytes / (1024 * 1024 * 1024));
    info!("📦 Block Size: {}MB", config.cache_block_size / (1024 * 1024));
    info!("🔄 Readahead Size: {}MB", config.readahead_bytes / (1024 * 1024));
    info!("⚡ Max Concurrent Tasks: {}", config.max_concurrent_caching);
    info!("🔒 Filesystem Mode: READ-ONLY");
    info!("🚀 Performance Optimization: ENABLED (4MB blocks + zero-copy reads)");
    info!("📊 FUSE Optimizations: max_read={} ({}MB)", config.cache_block_size.min(16 * 1024 * 1024), config.cache_block_size.min(16 * 1024 * 1024) / (1024 * 1024));
    info!("════════════════════════════════════════════════════════");
    info!("🎯 Ready for high-performance caching with large block I/O!");
    info!("════════════════════════════════════════════════════════");
    
    // 验证配置
    if let Err(e) = validate_config(&config) {
        error!("Configuration validation failed: {}", e);
        process::exit(1);
    }
    
    // 验证挂载点
    if let Err(e) = validate_mountpoint(&mountpoint) {
        error!("Mount point validation failed: {}", e);
        process::exit(1);
    }
    
    // 创建文件系统实例
    let fs = match CacheFs::new(config.clone()) {
        Ok(fs) => fs,
        Err(e) => {
            error!("Failed to create filesystem: {}", e);
            process::exit(1);
        }
    };
    
    info!("Read-only filesystem created successfully");
    
    // 设置信号处理
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let fs_for_signal = Arc::new(fs);
    let fs_for_mount = Arc::clone(&fs_for_signal);
    
    // 处理 SIGINT 和 SIGTERM
    tokio::spawn(async move {
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        
        tokio::select! {
            _ = sigint.recv() => {
                info!("Received SIGINT, shutting down...");
                // 优雅关闭文件系统组件
                let _ = fs_for_signal.shutdown().await;
                let _ = tx.send(()).await;
            }
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down...");
                // 优雅关闭文件系统组件
                let _ = fs_for_signal.shutdown().await;
                let _ = tx.send(()).await;
            }
        }
    });
    
    // 挂载文件系统
    info!("Mounting filesystem...");
    
    // 克隆mountpoint以避免所有权问题
    let mountpoint_for_task = mountpoint.clone();
    
    // 启动挂载任务
    let mut mount_result = tokio::task::spawn_blocking(move || {
        // 一旦调用这个函数，挂载就会成功，并且函数会一直运行直到卸载
        // 注意：这里需要解引用Arc来获取CacheFs实例
        let fs_ref = Arc::try_unwrap(fs_for_mount).unwrap_or_else(|arc| (*arc).clone());
        fuser::mount2(fs_ref, &mountpoint_for_task, &mount_options)
    });
    
    // 等待一小段时间让挂载完成
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    
    // 通过检查 /proc/mounts 来验证挂载是否成功
    let mountpoint_str = mountpoint.to_string_lossy();
    let mount_check = tokio::process::Command::new("grep")
        .arg(&*mountpoint_str)
        .arg("/proc/mounts")
        .output()
        .await;
    
    match mount_check {
        Ok(output) if output.status.success() => {
            info!("✅ Filesystem mounted successfully at {}", mountpoint_str);
            info!("🚀 NFS-CacheFS is now running and ready to serve files");
            info!("📊 Performance monitoring is active - you'll see detailed cache logs");
            info!("💡 TIP: Run 'ls -la {}' to test file access", mountpoint_str);
        }
        _ => {
            // 如果检查失败，可能还在挂载中，等待更长时间
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
            info!("📁 Filesystem mounting initiated at {}", mountpoint_str);
            info!("🔄 NFS-CacheFS is now running (mount verification may take a moment)");
            info!("📊 Performance monitoring is active - you'll see detailed cache logs");
        }
    }
    
    // 等待挂载任务完成或信号
    tokio::select! {
        result = &mut mount_result => {
            match result {
                Ok(Ok(())) => {
                    info!("Filesystem unmounted cleanly");
                }
                Ok(Err(e)) => {
                    error!("Filesystem error: {}", e);
                    process::exit(1);
                }
                Err(e) => {
                    error!("Mount task panicked: {}", e);
                    process::exit(1);
                }
            }
        }
        _ = rx.recv() => {
            info!("Received shutdown signal, unmounting...");
            // 主动卸载文件系统
            let _ = unmount_filesystem(&mountpoint).await;
            // 等待挂载任务完成
            match mount_result.await {
                Ok(Ok(())) => {
                    info!("Filesystem unmounted cleanly");
                }
                Ok(Err(e)) => {
                    warn!("Filesystem unmount error: {}", e);
                }
                Err(e) => {
                    warn!("Mount task error during shutdown: {}", e);
                }
            }
        }
    }
    
    info!("NFS-CacheFS shutdown complete");
}

/// 异步卸载文件系统
async fn unmount_filesystem(mountpoint: &std::path::PathBuf) -> Result<(), String> {
    let mountpoint_str = mountpoint.to_string_lossy();
    
    // 首先尝试正常卸载
    let result = tokio::process::Command::new("fusermount")
        .arg("-u")
        .arg(&*mountpoint_str)
        .output()
        .await;
    
    match result {
        Ok(output) if output.status.success() => {
            info!("Successfully unmounted filesystem at {}", mountpoint_str);
            return Ok(());
        }
        Ok(output) => {
            warn!("fusermount failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        Err(e) => {
            warn!("Failed to run fusermount: {}", e);
        }
    }
    
    // 如果fusermount失败，尝试使用umount
    let result = tokio::process::Command::new("umount")
        .arg(&*mountpoint_str)
        .output()
        .await;
    
    match result {
        Ok(output) if output.status.success() => {
            info!("Successfully unmounted filesystem at {} using umount", mountpoint_str);
            Ok(())
        }
        Ok(output) => {
            let error_msg = format!("umount failed: {}", String::from_utf8_lossy(&output.stderr));
            warn!("{}", error_msg);
            Err(error_msg)
        }
        Err(e) => {
            let error_msg = format!("Failed to run umount: {}", e);
            warn!("{}", error_msg);
            Err(error_msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    
    #[test]
    fn test_config_validation() {
        let temp_dir = TempDir::new().unwrap();
        let nfs_backend = temp_dir.path().join("nfs");
        let cache_dir = temp_dir.path().join("cache");
        
        // 创建 NFS 后端目录
        fs::create_dir(&nfs_backend).unwrap();
        
        let config = Config {
            nfs_backend_path: nfs_backend.clone(),
            cache_dir: cache_dir.clone(),
            mount_point: temp_dir.path().join("mount"),
            max_cache_size_bytes: 10 * 1024 * 1024 * 1024,
            cache_block_size: 64 * 1024 * 1024,
            max_concurrent_caching: 10,
            enable_checksums: true,
            cache_ttl_seconds: None,
            eviction_policy: nfs_cachefs::core::config::EvictionPolicy::Lru,
            direct_io: true,
            readahead_bytes: 1024 * 1024,
            min_cache_file_size: 100 * 1024 * 1024,
            allow_async_read: false,
            smart_cache: nfs_cachefs::core::config::SmartCacheConfig::default(),
        };
        
        // 验证应该成功
        assert!(validate_config(&config).is_ok());
        
        // 验证缓存目录是否被创建
        assert!(cache_dir.exists());
        
        // 测试无效配置
        let invalid_config = Config {
            nfs_backend_path: temp_dir.path().join("nonexistent"),
            cache_dir: cache_dir.clone(),
            mount_point: temp_dir.path().join("mount"),
            max_cache_size_bytes: 0,
            cache_block_size: 64 * 1024 * 1024,
            max_concurrent_caching: 10,
            enable_checksums: true,
            cache_ttl_seconds: None,
            eviction_policy: nfs_cachefs::core::config::EvictionPolicy::Lru,
            direct_io: true,
            readahead_bytes: 1024 * 1024,
            min_cache_file_size: 100 * 1024 * 1024,
            allow_async_read: false,
            smart_cache: nfs_cachefs::core::config::SmartCacheConfig::default(),
        };
        
        assert!(validate_config(&invalid_config).is_err());
    }
    
    #[test]
    fn test_mountpoint_validation() {
        let temp_dir = TempDir::new().unwrap();
        let mountpoint = temp_dir.path().to_path_buf();
        
        // 空目录应该通过验证
        assert!(validate_mountpoint(&mountpoint).is_ok());
        
        // 不存在的目录应该失败
        let nonexistent = temp_dir.path().join("nonexistent");
        assert!(validate_mountpoint(&nonexistent).is_err());
        
        // 文件而不是目录应该失败
        let file_path = temp_dir.path().join("file.txt");
        fs::write(&file_path, "test").unwrap();
        assert!(validate_mountpoint(&file_path).is_err());
    }
}
