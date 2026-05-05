//! TOML configuration. Mirrors `daemon.toml` schema documented in the
//! README; consumers should call [`Config::load`] and then convert to a
//! [`crate::proto::ConfigCmd`] via [`Config::as_config_cmd`].

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_brun")]
    pub brun: u8,
    #[serde(default = "default_bcull")]
    pub bcull: u8,
    #[serde(default = "default_bstop")]
    pub bstop: u8,
    #[serde(default = "default_brun")]
    pub frun: u8,
    #[serde(default = "default_bcull")]
    pub fcull: u8,
    #[serde(default = "default_bstop")]
    pub fstop: u8,
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

fn default_brun() -> u8 {
    10
}
fn default_bcull() -> u8 {
    7
}
fn default_bstop() -> u8 {
    3
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cull {
    /// Max objects to consider per cull pass. Bounds CPU and IO.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

impl Default for Cull {
    fn default() -> Self {
        Self {
            batch_size: default_batch_size(),
        }
    }
}

fn default_batch_size() -> usize {
    1024
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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

fn default_log_level() -> String {
    "info".into()
}
fn default_log_format() -> String {
    "compact".into()
}

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
        crate::proto::cmd::validate_config_args(
            &self.cache_dir,
            &self.tag,
            self.secctx.as_deref(),
        )?;
        let l = &self.limits;
        crate::proto::cmd::validate_limit_triplet("b", l.bstop, l.bcull, l.brun)?;
        crate::proto::cmd::validate_limit_triplet("f", l.fstop, l.fcull, l.frun)?;
        if self.cull.batch_size == 0 {
            return Err(Error::config("cull.batch_size must be > 0"));
        }
        if !matches!(
            self.log.level.as_str(),
            "error" | "warn" | "info" | "debug" | "trace"
        ) {
            return Err(Error::config(
                "log.level must be one of: error, warn, info, debug, trace",
            ));
        }
        if !matches!(self.log.format.as_str(), "compact" | "json") {
            return Err(Error::config("log.format must be compact or json"));
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

    #[test]
    fn rejects_run_at_100() {
        let s = r#"
            cache_dir = "/x"
            tag = "t"
            [limits]
            brun = 100
            bcull = 7
            bstop = 3
            frun = 10
            fcull = 7
            fstop = 3
        "#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_protocol_breaks_in_command_args() {
        let mut cfg = Config {
            cache_dir: PathBuf::from("/var/cache/fscache"),
            tag: "nfscache".into(),
            secctx: None,
            limits: Limits::default(),
            cull: Cull::default(),
            log: Log::default(),
        };

        cfg.tag = "bad\nbind".into();
        assert!(cfg.validate().is_err());

        cfg.tag = "nfscache".into();
        cfg.cache_dir = PathBuf::from("/var/cache/fs cache");
        assert!(cfg.validate().is_err());

        cfg.cache_dir = PathBuf::from("/var/cache/fscache");
        cfg.secctx = Some("ctx\nbind".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        let s = r#"
            cache_dir = "/var/cache/fscache"
            tag = "nfscache"
            max_size = "100G"
        "#;
        assert!(toml::from_str::<Config>(s).is_err());

        let s = r#"
            cache_dir = "/var/cache/fscache"
            tag = "nfscache"
            [limits]
            brun = 10
            bcull = 7
            bstop = 3
            unknown = 1
        "#;
        assert!(toml::from_str::<Config>(s).is_err());
    }

    #[test]
    fn rejects_bad_log_settings() {
        let s = r#"
            cache_dir = "/var/cache/fscache"
            tag = "nfscache"
            [log]
            level = "verbose"
            format = "compact"
        "#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert!(cfg.validate().is_err());

        let s = r#"
            cache_dir = "/var/cache/fscache"
            tag = "nfscache"
            [log]
            level = "info"
            format = "pretty"
        "#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert!(cfg.validate().is_err());
    }
}
