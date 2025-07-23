use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CacheFsError {
    #[error("NFS backend error: {0}")]
    NfsError(#[source] std::io::Error),
    
    #[error("Cache operation failed: {message}")]
    CacheError { message: String },
    
    #[error("Insufficient cache space: need {needed} bytes, available {available} bytes")]
    InsufficientSpace { needed: u64, available: u64 },
    
    #[error("Checksum mismatch for file: {path}")]
    ChecksumMismatch { path: PathBuf },
    
    #[error("Configuration error: {0}")]
    ConfigError(String),
    
    #[error("FUSE error: {0}")]
    FuseError(String),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    
    #[error("Parse error: {0}")]
    ParseError(#[from] std::num::ParseIntError),
    
    #[error("Parse bool error: {0}")]
    ParseBoolError(#[from] std::str::ParseBoolError),
    
    #[error("Path error: invalid path {path}")]
    PathError { path: String },
    
    #[error("Mount error: {message}")]
    MountError { message: String },
    
    #[error("Task join error: {0}")]
    TaskJoinError(#[from] tokio::task::JoinError),
    
    #[error("Send error: {0}")]
    SendError(String),
    
    #[error("Memory error: {0}")]
    MemoryError(String),
    
    #[error("Resource error: {0}")]
    ResourceError(String),
}

impl CacheFsError {
    pub fn cache_error(message: impl Into<String>) -> Self {
        Self::CacheError {
            message: message.into(),
        }
    }
    
    pub fn config_error(message: impl Into<String>) -> Self {
        Self::ConfigError(message.into())
    }
    
    pub fn fuse_error(message: impl Into<String>) -> Self {
        Self::FuseError(message.into())
    }
    
    pub fn mount_error(message: impl Into<String>) -> Self {
        Self::MountError {
            message: message.into(),
        }
    }
    
    pub fn path_error(path: impl Into<String>) -> Self {
        Self::PathError {
            path: path.into(),
        }
    }
    
    pub fn io_error(message: impl Into<String>) -> Self {
        Self::IoError(std::io::Error::new(
            std::io::ErrorKind::Other,
            message.into(),
        ))
    }
    
    pub fn memory_error(message: impl Into<String>) -> Self {
        Self::MemoryError(message.into())
    }
    
    pub fn resource_error(message: impl Into<String>) -> Self {
        Self::ResourceError(message.into())
    }
}

// 错误恢复策略
#[derive(Debug, Clone)]
pub enum RecoveryStrategy {
    Retry {
        max_attempts: u32,
        backoff_ms: u64,
    },
    Fallback, // 降级到直接NFS访问
    Fail,     // 向用户返回错误
}

impl Default for RecoveryStrategy {
    fn default() -> Self {
        Self::Retry {
            max_attempts: 3,
            backoff_ms: 1000,
        }
    }
} 