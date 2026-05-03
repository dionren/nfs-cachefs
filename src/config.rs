//! TOML configuration. Mirrors `daemon.toml` schema documented in the
//! README; consumers should call [`Config::load`] and then convert to a
//! [`crate::proto::ConfigCmd`] via [`Config::as_config_cmd`].

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub cache_dir: PathBuf,
    pub tag: String,
    pub secctx: Option<String>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub cull: Cull,
    #[serde(default)]
    pub log: Log,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Limits {
    #[serde(default = "default_brun")]  pub brun: u8,
    #[serde(default = "default_bcull")] pub bcull: u8,
    #[serde(default = "default_bstop")] pub bstop: u8,
    #[serde(default = "default_brun")]  pub frun: u8,
    #[serde(default = "default_bcull")] pub fcull: u8,
    #[serde(default = "default_bstop")] pub fstop: u8,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            brun: default_brun(),
            bcull: default_bcull(),
            bstop: default_bstop(),
            frun: default_brun(),
            fcull: default_bcull(),
            fstop: default_bstop(),
        }
    }
}

fn default_brun()  -> u8 { 10 }
fn default_bcull() -> u8 { 7 }
fn default_bstop() -> u8 { 3 }

#[derive(Debug, Clone, Deserialize)]
pub struct Cull {
    /// Max objects to consider per cull pass. Bounds CPU and IO.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

impl Default for Cull {
    fn default() -> Self {
        Self { batch_size: default_batch_size() }
    }
}

fn default_batch_size() -> usize { 1024 }

#[derive(Debug, Clone, Deserialize)]
pub struct Log {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_format")]
    pub format: String,
}

impl Default for Log {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
        }
    }
}

fn default_log_level()  -> String { "info".into() }
fn default_log_format() -> String { "compact".into() }

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::ConfigFile {
            path: path.to_path_buf(),
            source: anyhow::Error::from(e),
        })?;
        let cfg: Config = toml::from_str(&text).map_err(|e| Error::ConfigFile {
            path: path.to_path_buf(),
            source: anyhow::Error::from(e),
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.tag.is_empty() || self.tag.contains(char::is_whitespace) {
            return Err(Error::config("tag must be non-empty and whitespace-free"));
        }
        let l = &self.limits;
        for (label, s, c, r) in [
            ("b", l.bstop, l.bcull, l.brun),
            ("f", l.fstop, l.fcull, l.frun),
        ] {
            if !(s < c && c < r && r <= 100) {
                return Err(Error::config(format!(
                    "{label}stop({s}) < {label}cull({c}) < {label}run({r}) <= 100 required"
                )));
            }
        }
        if self.cull.batch_size == 0 {
            return Err(Error::config("cull.batch_size must be > 0"));
        }
        Ok(())
    }

    pub fn as_config_cmd(&self) -> crate::proto::ConfigCmd<'_> {
        crate::proto::ConfigCmd {
            cache_dir: &self.cache_dir,
            tag: &self.tag,
            secctx: self.secctx.as_deref(),
            brun: self.limits.brun,
            bcull: self.limits.bcull,
            bstop: self.limits.bstop,
            frun: self.limits.frun,
            fcull: self.limits.fcull,
            fstop: self.limits.fstop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let s = r#"
            cache_dir = "/var/cache/fscache"
            tag = "nfscache"
        "#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert_eq!(cfg.tag, "nfscache");
        assert_eq!(cfg.limits.bcull, 7);
        cfg.validate().unwrap();
    }

    #[test]
    fn rejects_bad_limits() {
        let s = r#"
            cache_dir = "/x"
            tag = "t"
            [limits]
            brun = 5
            bcull = 7
            bstop = 3
            frun = 10
            fcull = 7
            fstop = 3
        "#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert!(cfg.validate().is_err());
    }
}
