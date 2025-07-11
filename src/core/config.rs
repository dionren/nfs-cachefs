use std::path::PathBuf;
use std::str::FromStr;
use serde::{Deserialize, Serialize};
use crate::core::error::CacheFsError;
use crate::{DEFAULT_CACHE_BLOCK_SIZE, DEFAULT_MAX_CONCURRENT, DEFAULT_READAHEAD_SIZE};

/// 智能分块和零拷贝选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartCacheConfig {
    /// 小文件阈值（字节）- 小于此大小的文件将一次性读取
    pub small_file_threshold: u64,
    /// 零拷贝读取阈值（字节）- 大于此大小的文件使用零拷贝读取
    pub zero_copy_threshold: u64,
    /// 启用智能缓存策略
    pub enable_smart_caching: bool,
    /// 启用零拷贝穿透读取
    pub enable_zero_copy_read: bool,
    /// 启用流式读取大文件
    pub use_streaming_for_large_files: bool,
    /// 流式读取缓冲区大小
    pub streaming_buffer_size: usize,
}

impl Default for SmartCacheConfig {
    fn default() -> Self {
        Self {
            small_file_threshold: 1024 * 1024, // 1MB
            zero_copy_threshold: 4 * 1024 * 1024, // 4MB
            enable_smart_caching: true,
            enable_zero_copy_read: true,
            use_streaming_for_large_files: true,
            streaming_buffer_size: 16 * 1024 * 1024, // 16MB buffer
        }
    }
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

// 🚀 新增NVMe优化配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvmeConfig {
    pub use_io_uring: bool,
    pub queue_depth: u32,
    pub use_memory_mapping: bool,
    pub use_hugepages: bool,
    pub direct_io: bool,
    pub polling_mode: bool,
    pub numa_aware: bool,
}

impl Default for NvmeConfig {
    fn default() -> Self {
        Self {
            use_io_uring: false,      // 默认关闭，需要显式启用
            queue_depth: 128,         // NVMe队列深度
            use_memory_mapping: true,
            use_hugepages: false,     // 需要系统支持
            direct_io: true,
            polling_mode: false,      // 轮询模式，减少中断开销
            numa_aware: false,        // NUMA感知内存分配
        }
    }
}

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
    pub allow_async_read: bool,   // 是否允许异步读取（false=直接同步读取缓存，性能更好）
    pub smart_cache: SmartCacheConfig,  // 智能缓存配置
    pub nvme: NvmeConfig,        // NVMe优化配置
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
            allow_async_read: false,
            smart_cache: SmartCacheConfig::default(),
            nvme: NvmeConfig::default(),
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
                    "allow_async_read" => {
                        config.allow_async_read = value.parse()?;
                    }
                    "smart_cache_small_file_threshold_mb" => {
                        let mb: u64 = value.parse()?;
                        config.smart_cache.small_file_threshold = mb * 1024 * 1024;
                    }
                    "smart_cache_zero_copy_threshold_mb" => {
                        let mb: u64 = value.parse()?;
                        config.smart_cache.zero_copy_threshold = mb * 1024 * 1024;
                    }
                    "smart_cache_enable_smart_cache" => {
                        config.smart_cache.enable_smart_caching = value.parse()?;
                    }
                    "smart_cache_enable_zero_copy_read" => {
                        config.smart_cache.enable_zero_copy_read = value.parse()?;
                    }
                    "smart_cache_use_streaming_for_large_files" => {
                        config.smart_cache.use_streaming_for_large_files = value.parse()?;
                    }
                    "smart_cache_streaming_buffer_size_mb" => {
                        let mb: usize = value.parse()?;
                        config.smart_cache.streaming_buffer_size = mb * 1024 * 1024;
                    }
                    "nvme_use_io_uring" => {
                        config.nvme.use_io_uring = value.parse()?;
                    }
                    "nvme_queue_depth" => {
                        config.nvme.queue_depth = value.parse()?;
                    }
                    "nvme_use_memory_mapping" => {
                        config.nvme.use_memory_mapping = value.parse()?;
                    }
                    "nvme_use_hugepages" => {
                        config.nvme.use_hugepages = value.parse()?;
                    }
                    "nvme_direct_io" => {
                        config.nvme.direct_io = value.parse()?;
                    }
                    "nvme_polling_mode" => {
                        config.nvme.polling_mode = value.parse()?;
                    }
                    "nvme_numa_aware" => {
                        config.nvme.numa_aware = value.parse()?;
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