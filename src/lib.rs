pub mod core;
pub mod cache;
pub mod fs;
pub mod utils;

// 重新导出主要类型
pub use core::config::Config;
pub use core::error::CacheFsError;
pub use fs::cachefs::CacheFs;
pub use cache::state::{CacheStatus, CacheEntry};

// 常量定义
pub const FUSE_ROOT_ID: u64 = 1;
pub const DEFAULT_CACHE_BLOCK_SIZE: usize = 1024 * 1024; // 1MB
pub const DEFAULT_MAX_CONCURRENT: u32 = 4;
pub const DEFAULT_READAHEAD_SIZE: usize = 4 * 1024 * 1024; // 4MB

// 结果类型别名
pub type Result<T> = std::result::Result<T, CacheFsError>; 