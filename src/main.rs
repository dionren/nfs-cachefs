// use std::env;
use std::path::PathBuf;
use std::process;

use clap::{Arg, Command};
use fuser::MountOption;
use tracing::{error, info, warn};
use tracing_subscriber;

use nfs_cachefs::core::config::Config;
use nfs_cachefs::fs::cachefs::CacheFs;

/// 解析命令行参数
fn parse_args() -> (Config, PathBuf, Vec<MountOption>) {
    let matches = Command::new("nfs-cachefs")
        .version("0.1.0")
        .author("NFS-CacheFS Team")
        .about("High-performance asynchronous caching filesystem for NFS")
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
        .get_matches();

    let nfs_backend = PathBuf::from(matches.get_one::<String>("nfs_backend").unwrap());
    let mountpoint = PathBuf::from(matches.get_one::<String>("mountpoint").unwrap());
    
    // 解析挂载选项
    let mut mount_options = Vec::new();
    let mut config_options = std::collections::HashMap::new();
    
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
                    "ro" => mount_options.push(MountOption::RO),
                    "rw" => mount_options.push(MountOption::RW),
                    "allow_other" => mount_options.push(MountOption::AllowOther),
                    "allow_root" => mount_options.push(MountOption::AllowRoot),
                    "auto_unmount" => mount_options.push(MountOption::AutoUnmount),
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
        readahead_bytes: 1024 * 1024,
    };
    
    // 添加默认挂载选项
    if matches.get_flag("foreground") {
        mount_options.push(MountOption::CUSTOM("foreground".to_string()));
    }
    
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
    
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .with_thread_ids(true)
        .with_line_number(true)
        .init();
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
    // 解析命令行参数
    let (config, mountpoint, mount_options) = parse_args();
    
    // 初始化日志系统
    init_logging("info");
    
    info!("Starting NFS-CacheFS v0.1.0");
    info!("NFS Backend: {}", config.nfs_backend_path.display());
    info!("Cache Directory: {}", config.cache_dir.display());
    info!("Mount Point: {}", mountpoint.display());
    info!("Cache Size: {}GB", config.max_cache_size_bytes / (1024 * 1024 * 1024));
    info!("Block Size: {}MB", config.cache_block_size / (1024 * 1024));
    info!("Max Concurrent Tasks: {}", config.max_concurrent_caching);
    
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
    
    info!("Filesystem created successfully");
    
    // 设置信号处理
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    
    // 处理 SIGINT 和 SIGTERM
    tokio::spawn(async move {
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        
        tokio::select! {
            _ = sigint.recv() => {
                info!("Received SIGINT, shutting down...");
                let _ = tx.send(()).await;
            }
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down...");
                let _ = tx.send(()).await;
            }
        }
    });
    
    // 挂载文件系统
    info!("Mounting filesystem...");
    
    let mount_result = tokio::task::spawn_blocking(move || {
        fuser::mount2(fs, &mountpoint, &mount_options)
    });
    
    // 等待挂载完成或信号
    tokio::select! {
        result = mount_result => {
            match result {
                Ok(Ok(())) => {
                    info!("Filesystem mounted successfully");
                }
                Ok(Err(e)) => {
                    error!("Failed to mount filesystem: {}", e);
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
            // 在实际实现中，这里应该优雅地卸载文件系统
            // 由于 fuser::mount2 是阻塞的，我们需要其他方式来处理卸载
        }
    }
    
    info!("NFS-CacheFS shutdown complete");
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
