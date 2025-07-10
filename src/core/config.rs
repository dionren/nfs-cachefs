use std::path::PathBuf;
use std::str::FromStr;
use serde::{Deserialize, Serialize};
use crate::core::error::CacheFsError;
use crate::{DEFAULT_CACHE_BLOCK_SIZE, DEFAULT_MAX_CONCURRENT, DEFAULT_READAHEAD_SIZE};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub nfs_backend_path: PathBuf,
    pub cache_dir: PathBuf,
    pub mount_point: PathBuf,
    pub max_cache_size_bytes: u64,
    
    pub cache_block_size: usize,
    pub max_concurrent_caching: u32,
    pub enable_checksums: bool,
    pub cache_ttl_seconds: Option<u64>,
    pub direct_io: bool,
    pub readahead_bytes: usize,
    pub eviction_policy: EvictionPolicy,
    pub min_cache_file_size: u64,  // 最小缓存文件大小（字节）
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvictionPolicy {
    Lru,
    Lfu,
    Arc,
}

impl FromStr for EvictionPolicy {
    type Err = CacheFsError;
    
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "lru" => Ok(Self::Lru),
            "lfu" => Ok(Self::Lfu),
            "arc" => Ok(Self::Arc),
            _ => Err(CacheFsError::config_error(format!("Unknown eviction policy: {}", s))),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            nfs_backend_path: PathBuf::new(),
            cache_dir: PathBuf::new(),
            mount_point: PathBuf::new(),
            max_cache_size_bytes: 50 * 1024 * 1024 * 1024, // 50GB
            cache_block_size: DEFAULT_CACHE_BLOCK_SIZE,
            max_concurrent_caching: DEFAULT_MAX_CONCURRENT,
            enable_checksums: false,
            cache_ttl_seconds: None,
            direct_io: true,
            readahead_bytes: DEFAULT_READAHEAD_SIZE,
            eviction_policy: EvictionPolicy::Lru,
            min_cache_file_size: 100 * 1024 * 1024, // 100MB
        }
    }
}

impl Config {
    /// 从 FUSE mount 选项解析配置
    pub fn from_mount_options(options: &[&str], mount_point: PathBuf) -> Result<Self, CacheFsError> {
        let mut config = Config::default();
        config.mount_point = mount_point;
        
        for option in options {
            if let Some((key, value)) = option.split_once('=') {
                match key {
                    "nfs_backend" => {
                        config.nfs_backend_path = PathBuf::from(value);
                    }
                    "cache_dir" => {
                        config.cache_dir = PathBuf::from(value);
                    }
                    "cache_size_gb" => {
                        let gb: u64 = value.parse()?;
                        config.max_cache_size_bytes = gb * 1024 * 1024 * 1024;
                    }
                    "block_size_mb" => {
                        let mb: usize = value.parse()?;
                        config.cache_block_size = mb * 1024 * 1024;
                    }
                    "max_concurrent" => {
                        config.max_concurrent_caching = value.parse()?;
                    }
                    "checksum" => {
                        config.enable_checksums = value.parse()?;
                    }
                    "ttl_hours" => {
                        let hours: u64 = value.parse()?;
                        config.cache_ttl_seconds = Some(hours * 3600);
                    }
                    "direct_io" => {
                        config.direct_io = value.parse()?;
                    }
                    "readahead_mb" => {
                        let mb: usize = value.parse()?;
                        config.readahead_bytes = mb * 1024 * 1024;
                    }
                    "eviction" => {
                        config.eviction_policy = value.parse()?;
                    }
                    "min_cache_file_size_mb" => {
                        let mb: u64 = value.parse()?;
                        config.min_cache_file_size = mb * 1024 * 1024;
                    }
                    _ => {
                        // 忽略未知选项（如 allow_other 等 FUSE 标准选项）
                        tracing::debug!("Ignoring unknown mount option: {}", key);
                    }
                }
            }
        }
        
        // 验证必需参数
        if config.nfs_backend_path.as_os_str().is_empty() {
            return Err(CacheFsError::config_error("Missing required option: nfs_backend"));
        }
        if config.cache_dir.as_os_str().is_empty() {
            return Err(CacheFsError::config_error("Missing required option: cache_dir"));
        }
        
        // 验证路径存在性
        if !config.nfs_backend_path.exists() {
            return Err(CacheFsError::config_error(format!(
                "NFS backend path does not exist: {}",
                config.nfs_backend_path.display()
            )));
        }
        
        // 确保缓存目录存在
        if let Err(e) = std::fs::create_dir_all(&config.cache_dir) {
            return Err(CacheFsError::config_error(format!(
                "Failed to create cache directory {}: {}",
                config.cache_dir.display(),
                e
            )));
        }
        
        // 验证配置合理性
        if config.max_cache_size_bytes == 0 {
            return Err(CacheFsError::config_error("Cache size must be greater than 0"));
        }
        
        if config.cache_block_size == 0 {
            return Err(CacheFsError::config_error("Block size must be greater than 0"));
        }
        
        if config.max_concurrent_caching == 0 {
            return Err(CacheFsError::config_error("Max concurrent caching must be greater than 0"));
        }
        
        tracing::info!("Configuration loaded successfully: {:?}", config);
        Ok(config)
    }
    
    /// 获取缓存目录的可用空间
    pub fn get_available_cache_space(&self) -> Result<u64, CacheFsError> {
        let _metadata = std::fs::metadata(&self.cache_dir)?;
        let stat = nix::sys::statvfs::statvfs(&self.cache_dir)
            .map_err(|e| CacheFsError::cache_error(format!("Failed to get filesystem stats: {}", e)))?;
        
        let available_bytes = stat.blocks_available() * stat.block_size();
        Ok(available_bytes)
    }
    
    /// 验证缓存空间是否足够
    pub fn validate_cache_space(&self) -> Result<(), CacheFsError> {
        let available = self.get_available_cache_space()?;
        if available < self.max_cache_size_bytes {
            return Err(CacheFsError::InsufficientSpace {
                needed: self.max_cache_size_bytes,
                available,
            });
        }
        Ok(())
    }
} 