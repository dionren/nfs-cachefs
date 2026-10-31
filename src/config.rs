//! TOML configuration. Mirrors `daemon.toml` schema documented in the
//! README; consumers should call [`Config::load`] and then convert to a
//! [`crate::proto::ConfigCmd`] via [`Config::as_config_cmd`].

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub cache_dir: PathBuf,
    pub tag: String,
    pub secctx: Option<String>,
    /// Size budget for the cache, enforced as a project quota on
    /// `cache_dir` (see `quota.rs`). Unset: the cache may use the whole
    /// filesystem, as shaped by the `[limits]` percentages.
    #[serde(default)]
    pub max_size: Option<ByteSize>,
    /// Project ID that carries `max_size`. Nothing else on the filesystem
    /// may use it.
    #[serde(default = "default_quota_project_id")]
    pub quota_project_id: u32,
    /// XFS extent size hint inherited by new cache files. Unset: leave the
    /// cache directories as they are.
    #[serde(default)]
    pub extsize: Option<ByteSize>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub cull: Cull,
    #[serde(default)]
    pub metrics: Metrics,
    #[serde(default)]
    pub log: Log,
}

/// Default project ID for `max_size`: "nfcs" in ASCII. Far above the small
/// sequential IDs that docker and `xfs_quota` setups hand out.
pub const DEFAULT_QUOTA_PROJECT_ID: u32 = 0x6e66_6373;

fn default_quota_project_id() -> u32 {
    DEFAULT_QUOTA_PROJECT_ID
}

/// Smallest accepted `max_size`. The kernel turns the percentages into
/// blocks with integer division, so tiny budgets give meaningless limits.
pub const MIN_MAX_SIZE: u64 = 64 << 20;

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
    /// Oldest objects collected per walk of the cache directory when
    /// nothing is known yet about object sizes. Walks grow up to 64 times
    /// this when a pass needs to free more than one batch holds.
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
pub struct Metrics {
    /// Prometheus text file for node_exporter's textfile collector. Unset:
    /// no metrics are written.
    pub textfile: Option<PathBuf>,
    /// How often to rewrite the file.
    #[serde(default = "default_metrics_interval")]
    pub interval_secs: u64,
    /// How often to walk the cache to report its size when no `max_size`
    /// quota reports it for free. 0 disables the walk.
    #[serde(default = "default_usage_scan_interval")]
    pub usage_scan_interval_secs: u64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            textfile: None,
            interval_secs: default_metrics_interval(),
            usage_scan_interval_secs: default_usage_scan_interval(),
        }
    }
}

fn default_metrics_interval() -> u64 {
    15
}
fn default_usage_scan_interval() -> u64 {
    300
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

/// Log levels accepted in the config and on the command line.
pub const LOG_LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

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
        if !self.cache_dir.is_absolute() {
            return Err(Error::config("cache_dir must be an absolute path"));
        }
        let l = &self.limits;
        crate::proto::cmd::validate_limit_triplet("b", l.bstop, l.bcull, l.brun)?;
        crate::proto::cmd::validate_limit_triplet("f", l.fstop, l.fcull, l.frun)?;
        if let Some(size) = self.max_size {
            if size.0 < MIN_MAX_SIZE {
                return Err(Error::config(format!(
                    "max_size must be at least {}",
                    ByteSize(MIN_MAX_SIZE)
                )));
            }
        }
        if self.quota_project_id == 0 || self.quota_project_id == u32::MAX {
            return Err(Error::config(
                "quota_project_id must be between 1 and 4294967294",
            ));
        }
        if let Some(extsize) = self.extsize {
            if extsize.0 < 4096 || extsize.0 > 1 << 30 || extsize.0 % 4096 != 0 {
                return Err(Error::config(
                    "extsize must be a multiple of 4KiB between 4KiB and 1GiB",
                ));
            }
        }
        if self.cull.batch_size == 0 {
            return Err(Error::config("cull.batch_size must be > 0"));
        }
        self.metrics.validate()?;
        if !LOG_LEVELS.contains(&self.log.level.as_str()) {
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

impl Metrics {
    fn validate(&self) -> Result<()> {
        if let Some(path) = &self.textfile {
            if !path.is_absolute() {
                return Err(Error::config("metrics.textfile must be an absolute path"));
            }
            // node_exporter's textfile collector only reads `*.prom`; the
            // daemon writes a dot-prefixed temp file next to it and renames.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || path.extension().and_then(|e| e.to_str()) != Some("prom") {
                return Err(Error::config(
                    "metrics.textfile must name a *.prom file (node_exporter textfile collector)",
                ));
            }
        }
        if self.interval_secs == 0 {
            return Err(Error::config("metrics.interval_secs must be > 0"));
        }
        Ok(())
    }
}

/// A size in bytes. In TOML either an integer or a string with an optional
/// binary unit: `"512M"`, `"3T"`, `"1.5TiB"` (K = 1024).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "SizeRepr")]
pub struct ByteSize(pub u64);

#[derive(Deserialize)]
#[serde(untagged)]
enum SizeRepr {
    Int(u64),
    Str(String),
}

impl TryFrom<SizeRepr> for ByteSize {
    type Error = String;

    fn try_from(repr: SizeRepr) -> std::result::Result<Self, String> {
        match repr {
            SizeRepr::Int(n) => Ok(ByteSize(n)),
            SizeRepr::Str(s) => s.parse(),
        }
    }
}

impl FromStr for ByteSize {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let s = s.trim();
        let split = s
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(s.len());
        let (number, unit) = s.split_at(split);
        let shift = match unit.trim().to_ascii_uppercase().as_str() {
            "" | "B" => 0,
            "K" | "KB" | "KIB" => 10,
            "M" | "MB" | "MIB" => 20,
            "G" | "GB" | "GIB" => 30,
            "T" | "TB" | "TIB" => 40,
            "P" | "PB" | "PIB" => 50,
            other => return Err(format!("unknown size unit {other:?} in {s:?}")),
        };
        let too_big = || format!("size {s:?} is too large");
        if let Ok(n) = number.parse::<u64>() {
            return n
                .checked_mul(1u64 << shift)
                .map(ByteSize)
                .ok_or_else(too_big);
        }
        let value: f64 = number
            .parse()
            .map_err(|_| format!("invalid size {s:?}: expected a number with an optional unit"))?;
        let bytes = value * (1u64 << shift) as f64;
        if !bytes.is_finite() || bytes >= u64::MAX as f64 {
            return Err(too_big());
        }
        Ok(ByteSize(bytes.round() as u64))
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::util::human_bytes(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config {
            cache_dir: PathBuf::from("/var/cache/fscache"),
            tag: "nfscache".into(),
            secctx: None,
            max_size: None,
            quota_project_id: DEFAULT_QUOTA_PROJECT_ID,
            extsize: None,
            limits: Limits::default(),
            cull: Cull::default(),
            metrics: Metrics::default(),
            log: Log::default(),
        }
    }

    #[test]
    fn parses_minimal_config() {
        let s = r#"
            cache_dir = "/var/cache/fscache"
            tag = "nfscache"
        "#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert_eq!(cfg.tag, "nfscache");
        assert_eq!(cfg.limits.bcull, 7);
        assert_eq!(cfg.max_size, None);
        assert_eq!(cfg.quota_project_id, DEFAULT_QUOTA_PROJECT_ID);
        assert_eq!(cfg.metrics.textfile, None);
        cfg.validate().unwrap();
    }

    #[test]
    fn shipped_config_is_valid() {
        let text = include_str!("../packaging/etc/nfs-cachefs/daemon.toml");
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();

        // Every option the file shows commented out must parse too.
        let enabled: Vec<&str> = text
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest) if is_assignment(rest) => rest,
                _ => line,
            })
            .collect();
        let cfg: Config = toml::from_str(&enabled.join("\n")).unwrap();
        cfg.validate().unwrap();
        assert!(cfg.max_size.is_some() && cfg.extsize.is_some() && cfg.secctx.is_some());
        assert!(cfg.metrics.textfile.is_some());
    }

    /// `key = value` with a lowercase key.
    fn is_assignment(line: &str) -> bool {
        line.split_once(" = ").is_some_and(|(key, _)| {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        })
    }

    #[test]
    fn parses_budget_extsize_and_metrics() {
        let s = r#"
            cache_dir = "/mnt/nvme/nfs-cachefs"
            tag = "nfscache"
            max_size = "3T"
            quota_project_id = 4242
            extsize = "4M"
            [metrics]
            textfile = "/run/nfs-cachefs/nfs-cachefs.prom"
            interval_secs = 30
        "#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert_eq!(cfg.max_size, Some(ByteSize(3 << 40)));
        assert_eq!(cfg.quota_project_id, 4242);
        assert_eq!(cfg.extsize, Some(ByteSize(4 << 20)));
        assert_eq!(cfg.metrics.interval_secs, 30);
        assert_eq!(cfg.metrics.usage_scan_interval_secs, 300);
        cfg.validate().unwrap();

        let s = r#"
            cache_dir = "/x"
            tag = "t"
            max_size = 1099511627776
        "#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert_eq!(cfg.max_size, Some(ByteSize(1 << 40)));
    }

    #[test]
    fn byte_sizes() {
        let p = |s: &str| s.parse::<ByteSize>().map(|b| b.0);
        assert_eq!(p("4096"), Ok(4096));
        assert_eq!(p("512M"), Ok(512 << 20));
        assert_eq!(p("3T"), Ok(3 << 40));
        assert_eq!(p("3 TiB"), Ok(3 << 40));
        assert_eq!(p("1.5g"), Ok(3 << 29));
        assert_eq!(p("2KB"), Ok(2048));
        assert!(p("").is_err());
        assert!(p("T").is_err());
        assert!(p("3X").is_err());
        assert!(p("-1G").is_err());
        assert!(p("99999999999P").is_err());
        assert_eq!(ByteSize(3 << 40).to_string(), "3.0 TiB");
    }

    #[test]
    fn rejects_bad_budget_and_extsize() {
        let mut cfg = base();
        cfg.max_size = Some(ByteSize(1 << 20));
        assert!(cfg.validate().is_err());
        cfg.max_size = Some(ByteSize(1 << 40));
        cfg.validate().unwrap();

        cfg.quota_project_id = 0;
        assert!(cfg.validate().is_err());
        cfg.quota_project_id = u32::MAX;
        assert!(cfg.validate().is_err());
        cfg.quota_project_id = 7;

        cfg.extsize = Some(ByteSize(1000));
        assert!(cfg.validate().is_err());
        cfg.extsize = Some(ByteSize(2 << 30));
        assert!(cfg.validate().is_err());
        cfg.extsize = Some(ByteSize(1 << 20));
        cfg.validate().unwrap();
    }

    #[test]
    fn rejects_bad_metrics_settings() {
        let mut cfg = base();
        cfg.metrics.textfile = Some(PathBuf::from("relative.prom"));
        assert!(cfg.validate().is_err());
        cfg.metrics.textfile = Some(PathBuf::from("/run/x/metrics.txt"));
        assert!(cfg.validate().is_err());
        cfg.metrics.textfile = Some(PathBuf::from("/run/x/.hidden.prom"));
        assert!(cfg.validate().is_err());
        cfg.metrics.textfile = Some(PathBuf::from("/run/x/nfs-cachefs.prom"));
        cfg.validate().unwrap();
        cfg.metrics.interval_secs = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_relative_cache_dir() {
        let mut cfg = base();
        cfg.cache_dir = PathBuf::from("var/cache/fscache");
        assert!(cfg.validate().is_err());
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
        let mut cfg = base();

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
            max_bytes = "100G"
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

        let s = r#"
            cache_dir = "/var/cache/fscache"
            tag = "nfscache"
            [metrics]
            port = 9100
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
