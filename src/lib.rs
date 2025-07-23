pub mod core;
pub mod cache;
pub mod fs;
pub mod io;
pub mod utils;

// 重新导出主要类型
pub use core::config::Config;
pub use core::error::CacheFsError;
pub use fs::cachefs::CacheFs;
pub use cache::state::{CacheStatus, CacheEntry};

// 常量定义
pub const FUSE_ROOT_ID: u64 = 1;
// 优化后的默认配置参数
pub const DEFAULT_CACHE_BLOCK_SIZE: usize = 64 * 1024 * 1024; // 64MB - 优化大文件缓存性能
pub const DEFAULT_MAX_CONCURRENT: u32 = 10; // 10 - 增加并发度
pub const DEFAULT_READAHEAD_SIZE: usize = 32 * 1024 * 1024; // 32MB - 优化顺序读取

// 结果类型别名
pub type Result<T> = std::result::Result<T, CacheFsError>; 

 