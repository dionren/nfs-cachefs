//! Prometheus metrics for node_exporter's textfile collector.
//!
//! The daemon has no network access by design (`PrivateNetwork=`), so
//! instead of serving metrics it rewrites a `*.prom` file every
//! `metrics.interval_secs`: written next to the target under a dot name
//! the collector ignores, then renamed over it, so a scrape never sees a
//! partial file. The collector drops a file it cannot parse, so every
//! series here is unique and every label value escaped.

use std::collections::HashSet;
use std::fmt::{Display, Write as _};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use tracing::warn;

use crate::cull::{CullStats, FreeSpace, Usage};
use crate::proto::CacheState;
use crate::quota;
use crate::util::RateLimit;

/// Totals since the daemon started.
#[derive(Debug, Default, Clone)]
pub struct Counters {
    pub passes: u64,
    pub passes_without_progress: u64,
    pub culled_objects: u64,
    pub culled_bytes: u64,
    pub busy_skips: u64,
    pub errors: u64,
    pub graveyard_removed: u64,
    pub last_pass: Option<SystemTime>,
}

impl Counters {
    pub fn record_pass(&mut self, stats: &CullStats) {
        self.passes += 1;
        if !stats.made_progress() {
            self.passes_without_progress += 1;
        }
        self.culled_objects += stats.culled as u64;
        self.culled_bytes += stats.bytes_freed;
        self.busy_skips += stats.skipped_busy as u64;
        self.errors += (stats.errored + stats.kernel_eio) as u64;
        self.graveyard_removed += stats.graveyard_removed as u64;
        self.last_pass = Some(SystemTime::now());
    }

    pub fn record_graveyard(&mut self, stats: &CullStats) {
        self.graveyard_removed += stats.graveyard_removed as u64;
        self.errors += stats.errored as u64;
    }
}

/// What the daemon knows at one point in time.
#[derive(Debug, Default)]
pub struct Snapshot<'a> {
    pub tag: &'a str,
    pub state: Option<&'a CacheState>,
    pub space: Option<FreeSpace>,
    /// State letter of our record in `/proc/fs/fscache/caches`.
    pub cache_record: Option<&'a str>,
    pub usage: Option<Usage>,
    pub quota: Option<quota::Usage>,
    /// Contents of `/proc/fs/fscache/stats`.
    pub fscache_stats: Option<&'a str>,
}

pub struct MetricsFile {
    path: PathBuf,
    interval: Duration,
    next: Instant,
    failures: RateLimit,
}

impl MetricsFile {
    pub fn new(path: &Path, interval: Duration) -> Self {
        Self {
            path: path.to_path_buf(),
            interval,
            next: Instant::now(),
            failures: RateLimit::new(Duration::from_secs(600)),
        }
    }

    pub fn next_due(&self) -> Instant {
        self.next
    }

    pub fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// Rewrite the file. Failures are logged (rate-limited), never fatal.
    pub fn write(&mut self, snapshot: &Snapshot<'_>, counters: &Counters, now: Instant) {
        self.next = now + self.interval;
        if let Err(e) = self.replace(&render(snapshot, counters)) {
            if let Some(suppressed) = self.failures.allow(now) {
                warn!(
                    path = %self.path.display(),
                    error = %e,
                    suppressed,
                    "cannot write metrics file (is its directory in the unit's ReadWritePaths?)"
                );
            }
        }
    }

    /// Remove the file so a stale one does not outlive the daemon.
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    fn replace(&self, text: &str) -> io::Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("nfs-cachefs.prom");
        let tmp = dir.join(format!(".{name}.tmp"));
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        drop(file);
        std::fs::rename(&tmp, &self.path)
    }
}

pub fn render(s: &Snapshot<'_>, c: &Counters) -> String {
    let mut out = Out::default();
    out.gauge("up", "1 while the daemon runs.", 1);
    out.family(
        "build_info",
        "gauge",
        "Version and cache tag of the running daemon.",
    );
    out.sample(
        "build_info",
        &[("version", env!("CARGO_PKG_VERSION")), ("tag", s.tag)],
        1,
    );

    if let Some(record) = s.cache_record {
        out.gauge(
            "cache_active",
            "1 while the kernel reports the cache active (state A).",
            u8::from(record == "A"),
        );
        out.gauge(
            "cache_io_error",
            "1 once the kernel stopped the cache after an I/O error (state E).",
            u8::from(record == "E"),
        );
    }
    if let Some(state) = s.state {
        out.gauge(
            "culling",
            "1 while the kernel asks for culling.",
            u8::from(state.culling),
        );
    }
    if let Some(space) = s.space {
        let bs = space.block_size;
        out.gauge(
            "fs_size_bytes",
            "Size of the cache filesystem as statfs reports it (the project quota with max_size).",
            space.blocks_total.saturating_mul(bs),
        );
        out.gauge(
            "fs_avail_bytes",
            "Free space the kernel compares with the limits.",
            space.blocks_avail.saturating_mul(bs),
        );
        out.gauge(
            "fs_files",
            "Inodes of the cache filesystem.",
            space.files_total,
        );
        out.gauge("fs_files_free", "Free inodes.", space.files_free);
        if let Some(state) = s.state {
            out.family(
                "limit_bytes",
                "gauge",
                "Free-space limits: cull starts below cull, stops at run; caching stops below stop.",
            );
            for (name, blocks) in [
                ("run", state.brun),
                ("cull", state.bcull),
                ("stop", state.bstop),
            ] {
                out.sample("limit_bytes", &[("limit", name)], blocks.saturating_mul(bs));
            }
            out.family("limit_files", "gauge", "Free-inode limits.");
            for (name, files) in [
                ("run", state.frun),
                ("cull", state.fcull),
                ("stop", state.fstop),
            ] {
                out.sample("limit_files", &[("limit", name)], files);
            }
        }
    }
    if let Some(usage) = s.usage {
        out.gauge(
            "cache_objects",
            "Cache objects at the last walk of the cache.",
            usage.objects,
        );
        out.gauge(
            "cache_allocated_bytes",
            "Space the cache objects occupied at the last walk.",
            usage.allocated_bytes,
        );
    }
    if let Some(q) = s.quota {
        out.gauge(
            "quota_limit_bytes",
            "Block limit of the cache's project quota (max_size).",
            q.limit_bytes,
        );
        out.gauge(
            "quota_used_bytes",
            "Space charged to the cache's project quota.",
            q.used_bytes,
        );
        out.gauge(
            "quota_inodes",
            "Inodes charged to the cache's project quota.",
            q.inodes,
        );
    }

    out.counter("cull_passes_total", "Cull passes run.", c.passes);
    out.counter(
        "cull_passes_without_progress_total",
        "Cull passes that could free nothing.",
        c.passes_without_progress,
    );
    out.counter("culled_objects_total", "Objects culled.", c.culled_objects);
    out.counter(
        "culled_bytes_total",
        "Allocated bytes freed by culling, graveyard included.",
        c.culled_bytes,
    );
    out.counter(
        "cull_busy_skips_total",
        "Cull attempts the kernel refused because the object was in use.",
        c.busy_skips,
    );
    out.counter("cull_errors_total", "Per-object cull errors.", c.errors);
    out.counter(
        "graveyard_removed_total",
        "Graveyard entries deleted.",
        c.graveyard_removed,
    );
    if let Some(at) = c.last_pass {
        let secs = at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        out.gauge(
            "last_cull_pass_timestamp_seconds",
            "When the last cull pass finished.",
            format!("{secs:.3}"),
        );
    }

    if let Some(stats) = s.fscache_stats {
        let samples = parse_fscache_stats(stats);
        if !samples.is_empty() {
            out.family(
                "fscache_stat",
                "untyped",
                "Counters from /proc/fs/fscache/stats (system-wide).",
            );
            for (section, field, value) in samples {
                out.sample(
                    "fscache_stat",
                    &[("section", section), ("field", field)],
                    value,
                );
            }
        }
    }
    out.0
}

/// `(section, field, value)` from the `Section : key=value ...` lines of
/// `/proc/fs/fscache/stats`, each pair once.
fn parse_fscache_stats(text: &str) -> Vec<(&str, &str, u64)> {
    let mut seen = HashSet::new();
    let mut samples = Vec::new();
    for line in text.lines() {
        let Some((section, rest)) = line.split_once(':') else {
            continue;
        };
        let section = section.trim();
        if section.is_empty() || section.starts_with('-') {
            continue;
        }
        for token in rest.split_ascii_whitespace() {
            let Some((field, value)) = token.split_once('=') else {
                continue;
            };
            let Ok(value) = value.parse::<u64>() else {
                continue;
            };
            if seen.insert((section, field)) {
                samples.push((section, field, value));
            }
        }
    }
    samples
}

#[derive(Default)]
struct Out(String);

impl Out {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        let _ = writeln!(self.0, "# HELP nfs_cachefs_{name} {help}");
        let _ = writeln!(self.0, "# TYPE nfs_cachefs_{name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: impl Display) {
        let _ = write!(self.0, "nfs_cachefs_{name}");
        if !labels.is_empty() {
            self.0.push('{');
            for (i, (key, val)) in labels.iter().enumerate() {
                if i > 0 {
                    self.0.push(',');
                }
                let _ = write!(self.0, "{key}=\"{}\"", escape_label(val));
            }
            self.0.push('}');
        }
        let _ = writeln!(self.0, " {value}");
    }

    fn gauge(&mut self, name: &str, help: &str, value: impl Display) {
        self.family(name, "gauge", help);
        self.sample(name, &[], value);
    }

    fn counter(&mut self, name: &str, help: &str, value: u64) {
        self.family(name, "counter", help);
        self.sample(name, &[], value);
    }
}

fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FSCACHE_STATS: &str = "Netfs  : DR=0 DW=0 RA=1200 RP=0 WB=0 WBZ=0\n\
Netfs  : RD=700 rs=700 rf=0\n\
-- FS-Cache statistics --\n\
Cookies: n=40 v=1 vcol=0 voom=0\n\
NoSpace: nwr=0 ncr=0 cull=0\n\
IO     : rd=700 wr=500 mis=0\n\
IO     : rd=1\n";

    #[test]
    fn parses_fscache_stats_once_per_field() {
        let samples = parse_fscache_stats(FSCACHE_STATS);
        assert!(samples.contains(&("IO", "rd", 700)));
        assert!(samples.contains(&("NoSpace", "cull", 0)));
        assert!(samples.contains(&("Netfs", "RA", 1200)));
        assert_eq!(
            samples
                .iter()
                .filter(|(s, f, _)| *s == "IO" && *f == "rd")
                .count(),
            1
        );
    }

    #[test]
    fn renders_a_complete_snapshot() {
        let state = CacheState {
            culling: true,
            frun: 10,
            fcull: 7,
            fstop: 3,
            brun: 1000,
            bcull: 700,
            bstop: 300,
        };
        let snap = Snapshot {
            tag: "nfs\"cache",
            state: Some(&state),
            space: Some(FreeSpace {
                block_size: 4096,
                blocks_total: 10_000,
                blocks_avail: 650,
                files_total: 100,
                files_free: 50,
            }),
            cache_record: Some("A"),
            usage: Some(Usage {
                objects: 2,
                allocated_bytes: 8192,
                at: Instant::now(),
            }),
            quota: Some(quota::Usage {
                used_bytes: 1,
                inodes: 2,
                limit_bytes: 3,
            }),
            fscache_stats: Some(FSCACHE_STATS),
        };
        let counters = Counters {
            passes: 4,
            culled_bytes: 1 << 30,
            ..Counters::default()
        };
        let text = render(&snap, &counters);
        for line in [
            "nfs_cachefs_up 1",
            "nfs_cachefs_build_info{version=\"",
            "tag=\"nfs\\\"cache\"} 1",
            "nfs_cachefs_cache_active 1",
            "nfs_cachefs_cache_io_error 0",
            "nfs_cachefs_culling 1",
            "nfs_cachefs_fs_avail_bytes 2662400",
            "nfs_cachefs_limit_bytes{limit=\"cull\"} 2867200",
            "nfs_cachefs_limit_files{limit=\"stop\"} 3",
            "nfs_cachefs_cache_objects 2",
            "nfs_cachefs_quota_limit_bytes 3",
            "nfs_cachefs_cull_passes_total 4",
            "nfs_cachefs_culled_bytes_total 1073741824",
            "nfs_cachefs_fscache_stat{section=\"IO\",field=\"wr\"} 500",
            "# TYPE nfs_cachefs_cull_passes_total counter",
        ] {
            assert!(text.contains(line), "missing {line:?} in:\n{text}");
        }
        // Every sample line is unique (the collector rejects duplicates).
        let mut seen = HashSet::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let series = line.rsplit_once(' ').unwrap().0;
            assert!(seen.insert(series.to_string()), "duplicate series {series}");
        }
    }

    #[test]
    fn renders_without_optional_parts() {
        let text = render(&Snapshot::default(), &Counters::default());
        assert!(text.contains("nfs_cachefs_up 1"));
        assert!(!text.contains("nfs_cachefs_culling"));
        assert!(!text.contains("last_cull_pass"));
    }

    #[test]
    fn replaces_the_file_atomically() {
        let dir = std::env::temp_dir().join(format!("nfs-cachefs-metrics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nfs-cachefs.prom");
        let mut file = MetricsFile::new(&path, Duration::from_secs(15));
        let now = Instant::now();
        assert!(file.due(now));
        file.write(&Snapshot::default(), &Counters::default(), now);
        assert!(!file.due(now));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# HELP nfs_cachefs_up"));
        assert!(!dir.join(".nfs-cachefs.prom.tmp").exists());
        file.remove();
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
