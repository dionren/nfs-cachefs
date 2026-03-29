use std::collections::HashMap;
use std::path::PathBuf;
use std::process;
use std::env;

use clap::{Arg, Command};
use fuser::MountOption;
use tracing::{error, info, warn};

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

    if args.len() < 4 {
        return Err("Invalid mount helper arguments".to_string());
    }

    let _source = &args[1];
    let mountpoint = PathBuf::from(&args[2]);

    let mut mount_options = Vec::new();
    let mut config_options = HashMap::new();

    mount_options.push(MountOption::RO);

    let mut i = 3;
    while i < args.len() {
        if args[i] == "-o" && i + 1 < args.len() {
            let options_str = &args[i + 1];
            for option in options_str.split(',') {
                let option = option.trim();
                if option.is_empty() {
                    continue;
                }

                if let Some((key, value)) = option.split_once('=') {
                    config_options.insert(key.to_string(), value.to_string());
                } else {
                    match option {
                        "ro" => {}
                        "rw" => {
                            warn!("Read-write mode is not supported, filesystem will be mounted read-only");
                        }
                        "allow_other" => mount_options.push(MountOption::AllowOther),
                        "allow_root" => mount_options.push(MountOption::AllowRoot),
                        "auto_unmount" => mount_options.push(MountOption::AutoUnmount),
                        "foreground" | "fg" => {}
                        _ => {
                            mount_options.push(MountOption::CUSTOM(option.to_string()));
                        }
                    }
                }
            }
            break;
        }
        i += 1;
    }

    let nfs_backend = config_options.get("nfs_backend")
        .ok_or("Missing required option: nfs_backend")?;
    let nfs_backend_path = PathBuf::from(nfs_backend);

    let cache_dir = config_options.get("cache_dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/nfs-cachefs"));

    let cache_size_gb: u64 = config_options.get("cache_size_gb")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let block_size_mb: usize = config_options.get("block_size_mb")
        .and_then(|s| s.parse().ok())
        .unwrap_or(64);

    let max_concurrent_caching: u32 = config_options.get("max_concurrent")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let min_cache_file_size_mb: u64 = config_options.get("min_cache_file_size_mb")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);

    let max_read_mb = block_size_mb.min(16);
    mount_options.push(MountOption::CUSTOM(format!("max_read={}", max_read_mb * 1024 * 1024)));

    let readahead_bytes = max_read_mb * 2 * 1024 * 1024;

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
        readahead_bytes,
        min_cache_file_size: min_cache_file_size_mb * 1024 * 1024,
        allow_async_read: false,
        smart_cache: nfs_cachefs::core::config::SmartCacheConfig::default(),
        nvme: nfs_cachefs::core::config::NvmeConfig::default(),
    };

    Ok((config, mountpoint, mount_options))
}

/// 解析命令行参数
fn parse_args() -> (Config, PathBuf, Vec<MountOption>) {
    if is_mount_helper_mode() {
        match parse_mount_helper_args() {
            Ok(result) => return result,
            Err(e) => {
                eprintln!("Mount helper mode error: {}", e);
                process::exit(1);
            }
        }
    }

    let matches = Command::new("nfs-cachefs")
        .version("0.6.1")
        .author("NFS-CacheFS Team")
        .about("High-performance read-only asynchronous caching filesystem for NFS")
        .arg(Arg::new("nfs_backend").help("NFS backend directory path").required(true).index(1))
        .arg(Arg::new("mountpoint").help("Mount point directory").required(true).index(2))
        .arg(Arg::new("options").short('o').long("options").help("Mount options (comma-separated)").value_name("OPTIONS").action(clap::ArgAction::Set))
        .arg(Arg::new("cache_dir").long("cache-dir").help("Cache directory path").value_name("PATH").action(clap::ArgAction::Set))
        .arg(Arg::new("cache_size").long("cache-size").help("Cache size in GB").value_name("SIZE").action(clap::ArgAction::Set))
        .arg(Arg::new("block_size").long("block-size").help("Block size in MB").value_name("SIZE").action(clap::ArgAction::Set))
        .arg(Arg::new("max_concurrent_tasks").long("max-concurrent-tasks").help("Maximum concurrent caching tasks").value_name("COUNT").action(clap::ArgAction::Set))
        .arg(Arg::new("foreground").short('f').long("foreground").help("Run in foreground").action(clap::ArgAction::SetTrue))
        .arg(Arg::new("debug").short('d').long("debug").help("Enable debug logging").action(clap::ArgAction::SetTrue))
        .arg(Arg::new("min_cache_file_size").long("min-cache-file-size").help("Minimum file size to cache in MB (default: 100)").value_name("SIZE_MB").action(clap::ArgAction::Set))
        .get_matches();

    let nfs_backend = PathBuf::from(matches.get_one::<String>("nfs_backend").unwrap());
    let mountpoint = PathBuf::from(matches.get_one::<String>("mountpoint").unwrap());

    let mut mount_options = Vec::new();
    let mut config_options = HashMap::new();

    mount_options.push(MountOption::RO);

    if let Some(options_str) = matches.get_one::<String>("options") {
        for option in options_str.split(',') {
            let option = option.trim();
            if option.is_empty() { continue; }

            if let Some((key, value)) = option.split_once('=') {
                config_options.insert(key.to_string(), value.to_string());
            } else {
                match option {
                    "ro" => {}
                    "rw" => {
                        warn!("Read-write mode is not supported, filesystem will be mounted read-only");
                    }
                    "allow_other" => mount_options.push(MountOption::AllowOther),
                    "allow_root" => mount_options.push(MountOption::AllowRoot),
                    "auto_unmount" => mount_options.push(MountOption::AutoUnmount),
                    "foreground" | "fg" => {}
                    _ => {
                        mount_options.push(MountOption::CUSTOM(option.to_string()));
                    }
                }
            }
        }
    }

    let cache_dir = matches.get_one::<String>("cache_dir").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/nfs-cachefs"));

    let cache_size_gb: u64 = matches.get_one::<String>("cache_size")
        .and_then(|s| s.parse().ok()).unwrap_or(10);

    let block_size_mb: usize = matches.get_one::<String>("block_size")
        .and_then(|s| s.parse().ok()).unwrap_or(64);

    let max_concurrent_caching: u32 = matches.get_one::<String>("max_concurrent_tasks")
        .and_then(|s| s.parse().ok()).unwrap_or(10);

    let min_cache_file_size_mb: u64 = matches.get_one::<String>("min_cache_file_size")
        .and_then(|s| s.parse().ok()).unwrap_or(100);

    let max_read_mb = block_size_mb.min(16);
    mount_options.push(MountOption::CUSTOM(format!("max_read={}", max_read_mb * 1024 * 1024)));

    let readahead_bytes = max_read_mb * 2 * 1024 * 1024;

    let config = Config {
        nfs_backend_path: nfs_backend,
        cache_dir,
        mount_point: mountpoint.clone(),
        max_cache_size_bytes: cache_size_gb * 1024 * 1024 * 1024,
        cache_block_size: block_size_mb * 1024 * 1024,
        max_concurrent_caching,
        enable_checksums: true,
        cache_ttl_seconds: None,
        eviction_policy: nfs_cachefs::core::config::EvictionPolicy::Lru,
        direct_io: true,
        readahead_bytes,
        min_cache_file_size: min_cache_file_size_mb * 1024 * 1024,
        allow_async_read: false,
        smart_cache: nfs_cachefs::core::config::SmartCacheConfig::default(),
        nvme: nfs_cachefs::core::config::NvmeConfig::default(),
    };

    (config, mountpoint, mount_options)
}

/// 初始化日志系统
fn init_logging() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .with_thread_ids(false)
        .with_line_number(false)
        .with_level(true)
        .with_ansi(true)
        .compact()
        .init();
}

/// 验证配置
fn validate_config(config: &Config) -> Result<(), String> {
    if !config.nfs_backend_path.exists() {
        return Err(format!("NFS backend directory does not exist: {}", config.nfs_backend_path.display()));
    }

    if !config.nfs_backend_path.is_dir() {
        return Err(format!("NFS backend path is not a directory: {}", config.nfs_backend_path.display()));
    }

    if let Some(parent) = config.cache_dir.parent() {
        if !parent.exists() {
            return Err(format!("Cache directory parent does not exist: {}", parent.display()));
        }
    }

    if !config.cache_dir.exists() {
        std::fs::create_dir_all(&config.cache_dir)
            .map_err(|e| format!("Failed to create cache directory: {}", e))?;
    }

    if config.max_cache_size_bytes == 0 {
        return Err("Cache size must be greater than 0".to_string());
    }

    if config.cache_block_size == 0 || config.cache_block_size > 1024 * 1024 * 1024 {
        return Err("Block size must be between 1 and 1024 MB".to_string());
    }

    if config.max_concurrent_caching == 0 {
        return Err("Max concurrent tasks must be greater than 0".to_string());
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

    Ok(())
}

/// 主函数
///
/// 关键修复：先 fork() 守护进程化，再创建 Tokio 运行时。
/// 在多线程环境下调用 fork() 是未定义行为。
fn main() {
    // 1. 检查是否需要后台运行（在任何多线程操作之前）
    let args: Vec<String> = std::env::args().collect();
    let is_foreground = args.iter().any(|arg| arg == "--foreground" || arg == "-f") ||
                       args.iter().any(|arg| arg.contains("foreground"));

    if mount_helper::should_daemonize(&args) && !is_foreground {
        if let Err(e) = mount_helper::daemonize() {
            eprintln!("Failed to daemonize: {}", e);
            process::exit(1);
        }
    }

    // 2. 解析命令行参数（同步操作，不需要运行时）
    let (config, mountpoint, mount_options) = parse_args();

    // 3. 初始化日志
    init_logging();

    info!("Starting NFS-CacheFS v0.6.1 (READ-ONLY MODE)");
    info!("NFS Backend: {}", config.nfs_backend_path.display());
    info!("Cache Directory: {}", config.cache_dir.display());
    info!("Mount Point: {}", mountpoint.display());
    info!("Cache Size: {}GB", config.max_cache_size_bytes / (1024 * 1024 * 1024));
    info!("Block Size: {}MB", config.cache_block_size / (1024 * 1024));
    info!("Min Cache File Size: {}MB", config.min_cache_file_size / (1024 * 1024));
    info!("Max Concurrent Tasks: {}", config.max_concurrent_caching);

    // 4. 验证配置和挂载点
    if let Err(e) = validate_config(&config) {
        error!("Configuration validation failed: {}", e);
        process::exit(1);
    }

    if let Err(e) = validate_mountpoint(&mountpoint) {
        error!("Mount point validation failed: {}", e);
        process::exit(1);
    }

    // 5. 创建 Tokio 运行时（在 fork 之后，保证线程安全）
    let runtime = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let handle = runtime.handle().clone();

    // 6. 创建文件系统实例
    let fs = match CacheFs::new(config.clone(), handle) {
        Ok(fs) => fs,
        Err(e) => {
            error!("Failed to create filesystem: {}", e);
            process::exit(1);
        }
    };

    let fs_for_shutdown = fs.clone();

    // 7. 设置信号处理（在运行时中异步运行）
    let mountpoint_for_signal = mountpoint.clone();
    runtime.spawn(async move {
        let mut sigint = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::interrupt()
        ).expect("Failed to setup SIGINT handler");
        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate()
        ).expect("Failed to setup SIGTERM handler");

        tokio::select! {
            _ = sigint.recv() => info!("Received SIGINT, shutting down..."),
            _ = sigterm.recv() => info!("Received SIGTERM, shutting down..."),
        }

        // 卸载文件系统
        let mp = mountpoint_for_signal.to_string_lossy().to_string();
        let _ = tokio::process::Command::new("fusermount")
            .arg("-u")
            .arg(&mp)
            .output()
            .await;
    });

    // 8. 挂载文件系统（阻塞主线程直到卸载）
    info!("Mounting filesystem at {}...", mountpoint.display());
    match fuser::mount2(fs, &mountpoint, &mount_options) {
        Ok(()) => {
            info!("Filesystem unmounted cleanly");
        }
        Err(e) => {
            error!("Filesystem error: {}", e);
            process::exit(1);
        }
    }

    // 9. 优雅关闭
    runtime.block_on(async {
        let _ = fs_for_shutdown.shutdown().await;
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
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
            nvme: nfs_cachefs::core::config::NvmeConfig::default(),
        };

        assert!(validate_config(&config).is_ok());
        assert!(cache_dir.exists());

        let invalid_config = Config {
            nfs_backend_path: temp_dir.path().join("nonexistent"),
            ..config
        };

        assert!(validate_config(&invalid_config).is_err());
    }

    #[test]
    fn test_mountpoint_validation() {
        let temp_dir = TempDir::new().unwrap();
        let mountpoint = temp_dir.path().to_path_buf();

        assert!(validate_mountpoint(&mountpoint).is_ok());

        let nonexistent = temp_dir.path().join("nonexistent");
        assert!(validate_mountpoint(&nonexistent).is_err());

        let file_path = temp_dir.path().join("file.txt");
        fs::write(&file_path, "test").unwrap();
        assert!(validate_mountpoint(&file_path).is_err());
    }
}
