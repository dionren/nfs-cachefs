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
}

impl Error {
    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
