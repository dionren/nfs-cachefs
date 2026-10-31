use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("config: {0}")]
    Config(String),

    #[error("config file {path}: {source}")]
    ConfigFile {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },

    #[error("cachefiles protocol: {0}")]
    Protocol(String),

    #[error("kernel rejected command {cmd:?}: {source}")]
    Kernel {
        cmd: String,
        #[source]
        source: io::Error,
    },

    /// The kernel stopped the cache after an I/O error (state `E` in
    /// `/proc/fs/fscache/caches`). It caches nothing more until the daemon
    /// closes `/dev/cachefiles` and binds again.
    #[error("cache {tag:?} was stopped by the kernel after an I/O error")]
    CacheDead { tag: String },

    /// The stop signal arrived during startup work that runs before bind.
    #[error("interrupted by stop signal")]
    Interrupted,
}

impl Error {
    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// The errno behind a kernel or I/O error, if any.
    pub fn raw_os_error(&self) -> Option<i32> {
        match self {
            Self::Io(e) | Self::Kernel { source: e, .. } => e.raw_os_error(),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
