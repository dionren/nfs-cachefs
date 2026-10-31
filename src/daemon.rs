//! Main event loop. Polls `/dev/cachefiles`; on every wakeup reads the
//! kernel's state line and, while it asks for culling, lets the planner
//! (`planner.rs`) decide whether to run a cull pass and how big. Around
//! that it watches the cache's kernel state, empties the graveyard, feeds
//! the systemd watchdog and writes metrics.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::cull::{self, CullCtx, CullTable, FreeSpace, Usage};
use crate::error::{Error, Result};
use crate::metrics::{Counters, MetricsFile, Snapshot};
use crate::planner::{Decision, Planner};
use crate::proto::{CacheState, Device, Kernel};
use crate::systemd_notify::{self, Watchdog};
use crate::util::{human_bytes, RateLimit};
use crate::{fsattr, quota};

/// How often to log a heartbeat / metrics summary at INFO when idle.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// How often to drain cachefiles' graveyard even when the kernel is not asking
/// for active culling.
const GRAVEYARD_INTERVAL: Duration = Duration::from_secs(30);

/// How often to check the cache's state in `/proc/fs/fscache/caches`.
const HEALTH_INTERVAL: Duration = Duration::from_secs(10);

/// Longest poll. The kernel marks the device readable on state changes,
/// but waking up anyway re-reads the state (which makes the kernel
/// re-check free space) and runs the timers.
const POLL_IDLE: Duration = Duration::from_secs(5);

const CACHES_PROC: &str = "/proc/fs/fscache/caches";
const STATS_PROC: &str = "/proc/fs/fscache/stats";

pub struct Daemon<'a> {
    pub dev: Device,
    pub config: &'a Config,
    pub stop: &'static AtomicBool,
}

impl Daemon<'_> {
    /// Bind the cache and run until `stop` is set. Returns on graceful exit
    /// or fatal error.
    pub fn run(self) -> Result<()> {
        let cfg = self.config;
        preflight(&cfg.tag, &cfg.cache_dir);
        if let Some(max_size) = cfg.max_size {
            apply_budget(cfg, max_size.0, self.stop)?;
        }
        if let Some(extsize) = cfg.extsize {
            // Validated to fit: at most 1 GiB.
            let changed = fsattr::apply_extsize_hint(&cfg.cache_dir, extsize.0 as u32, self.stop)?;
            info!(extsize = %extsize, dirs_changed = changed, "extent size hint set on cache directories");
        }

        cfg.as_config_cmd().apply_and_bind(&self.dev)?;
        info!(
            tag = cfg.tag,
            cache_dir = %cfg.cache_dir.display(),
            "cache bound (traditional mode)"
        );
        match systemd_notify::ready("cache bound") {
            Ok(true) => debug!("sent systemd readiness notification"),
            Ok(false) => debug!("NOTIFY_SOCKET not set; readiness notification skipped"),
            Err(e) => {
                return Err(Error::Io(std::io::Error::other(format!(
                    "failed to notify systemd readiness: {e}"
                ))));
            }
        }

        let mut runner = Runner::new(cfg, &self.dev, self.stop);
        let result = runner.run();
        systemd_notify::stopping();
        if matches!(result, Err(Error::CacheDead { .. })) {
            // Leave the state behind for the scrapes until the restarted
            // daemon replaces it.
            runner.write_metrics(Instant::now());
        } else if let Some(metrics) = &runner.metrics {
            metrics.remove();
        }
        result
        // self.dev drops here, closing the fd; the kernel withdraws the cache.
    }
}

/// Put the cache under its `max_size` project quota before bind.
fn apply_budget(cfg: &Config, limit: u64, stop: &AtomicBool) -> Result<()> {
    let started = Instant::now();
    let mut last_extend = Instant::now();
    let budget = quota::Budget {
        project_id: cfg.quota_project_id,
        limit,
    };
    let applied = quota::apply(&cfg.cache_dir, budget, stop, |entries| {
        // Tagging a large cache for the first time can outlast the unit's
        // start timeout.
        if last_extend.elapsed() >= Duration::from_secs(5) {
            last_extend = Instant::now();
            systemd_notify::extend_start_timeout(
                Duration::from_secs(30),
                &format!("tagging cache for its project quota: {entries} entries"),
            );
        }
    })?;
    info!(
        project = cfg.quota_project_id,
        max_size = %human_bytes(limit),
        used = %human_bytes(applied.usage.used_bytes),
        entries = applied.entries,
        retagged = applied.tagged,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "cache size budget applied (project quota)"
    );
    Ok(())
}

/// Log rate limits for messages that could repeat every loop iteration.
struct Logs {
    no_progress: RateLimit,
    statfs: RateLimit,
    parse: RateLimit,
    record: RateLimit,
}

struct Runner<'a, K: Kernel> {
    cfg: &'a Config,
    dev: &'a K,
    stop: &'static AtomicBool,
    ctx: CullCtx,
    wd: Watchdog,
    planner: Planner,
    table: CullTable,
    counters: Counters,
    metrics: Option<MetricsFile>,
    last_state: Option<CacheState>,
    /// When to consult the planner again while culling; `None` = now.
    decide_at: Option<Instant>,
    next_health: Instant,
    next_graveyard: Instant,
    next_heartbeat: Instant,
    /// Cache size walks, only when metrics want a size no quota reports.
    next_usage: Option<Instant>,
    usage: Option<Usage>,
    record_state: Option<String>,
    /// `/proc/fs/fscache/caches`; a test points it elsewhere.
    caches_proc: &'static str,
    logs: Logs,
}

impl<'a, K: Kernel> Runner<'a, K> {
    fn new(cfg: &'a Config, dev: &'a K, stop: &'static AtomicBool) -> Self {
        let now = Instant::now();
        let metrics = cfg
            .metrics
            .textfile
            .as_deref()
            .map(|path| MetricsFile::new(path, Duration::from_secs(cfg.metrics.interval_secs)));
        let next_usage = (metrics.is_some()
            && cfg.max_size.is_none()
            && cfg.metrics.usage_scan_interval_secs > 0)
            .then_some(now);
        let limit = |minutes: u64| RateLimit::new(Duration::from_secs(60 * minutes));
        Self {
            cfg,
            dev,
            stop,
            ctx: CullCtx {
                cache_root: cfg.cache_dir.clone(),
                batch_size: cfg.cull.batch_size,
            },
            wd: Watchdog::from_env(),
            planner: Planner::default(),
            table: CullTable::default(),
            counters: Counters::default(),
            metrics,
            last_state: None,
            decide_at: None,
            next_health: now,
            next_graveyard: now + GRAVEYARD_INTERVAL,
            next_heartbeat: now + HEARTBEAT_INTERVAL,
            next_usage,
            usage: None,
            record_state: None,
            caches_proc: CACHES_PROC,
            logs: Logs {
                no_progress: limit(5),
                statfs: limit(5),
                parse: limit(5),
                record: limit(60),
            },
        }
    }

    fn run(&mut self) -> Result<()> {
        if let Some(interval) = self.wd.interval() {
            debug!(
                interval_ms = interval.as_millis() as u64,
                "systemd watchdog enabled"
            );
        }
        self.graveyard();
        let mut buf = [0u8; 256];

        while !self.stop.load(Ordering::Relaxed) {
            self.wd.kick();
            let timeout = self.poll_timeout(Instant::now());
            let mut pollfd = libc::pollfd {
                fd: self.dev.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // POLLIN only: the kernel also asserts POLLOUT for as long as
            // its cull flag is set, which would spin the loop whenever the
            // planner decides to wait.
            let r = unsafe { libc::poll(&mut pollfd, 1, timeout) };
            if r < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(Error::Io(err));
            }
            if r > 0 && (pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)) != 0 {
                return Err(Error::Io(std::io::Error::other(format!(
                    "/dev/cachefiles poll returned revents=0x{:x}",
                    pollfd.revents
                ))));
            }

            // Read on timeouts too. The kernel re-checks free space only when
            // the cache writes or the daemon reads, so otherwise an idle
            // cache is never culled while other data fills the filesystem,
            // and the heartbeat has no state to report.
            self.read_state(&mut buf)?;
            if let Some(state) = self.last_state {
                self.cull_step(state)?;
            }
            self.timers()?;
        }

        info!("stop signal received; closing /dev/cachefiles (kernel will unbind)");
        Ok(())
    }

    fn poll_timeout(&self, now: Instant) -> i32 {
        let culling = self.last_state.is_some_and(|s| s.culling);
        if culling && self.decide_at.is_none() {
            return 0;
        }
        let deadline = [
            Some(now + POLL_IDLE),
            self.decide_at,
            Some(self.next_health),
            Some(self.next_graveyard),
            Some(self.next_heartbeat),
            self.next_usage,
            self.metrics.as_ref().map(MetricsFile::next_due),
            self.wd.interval().map(|i| now + i / 3),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(now);
        i32::try_from(deadline.saturating_duration_since(now).as_millis()).unwrap_or(i32::MAX)
    }

    fn read_state(&mut self, buf: &mut [u8]) -> Result<()> {
        match self.dev.read_state(buf) {
            Ok(0) => {} // not ready yet
            Ok(n) => match CacheState::parse(&buf[..n]) {
                Ok(state) => {
                    debug!(?state, "state");
                    self.last_state = Some(state);
                }
                Err(e) => {
                    if let Some(suppressed) = self.logs.parse.allow(Instant::now()) {
                        warn!(error = %e, suppressed, "failed to parse state line");
                    }
                }
            },
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => {}
            Err(e) => {
                error!(error = %e, "read /dev/cachefiles failed");
                return Err(e);
            }
        }
        Ok(())
    }

    fn cull_step(&mut self, state: CacheState) -> Result<()> {
        if !state.culling {
            self.planner.idle();
            self.decide_at = None;
            return Ok(());
        }
        let now = Instant::now();
        if self.decide_at.is_some_and(|at| now < at) {
            return Ok(());
        }
        self.decide_at = None;

        let space = match FreeSpace::of(&self.ctx.cache_root) {
            Ok(space) => space,
            Err(e) => {
                let wait = self.planner.defer(now);
                self.decide_at = Some(now + wait);
                if let Some(suppressed) = self.logs.statfs.allow(now) {
                    warn!(error = %e, suppressed, retry_in_s = wait.as_secs(), "statfs on cache_dir failed; cannot size a cull pass");
                }
                return Ok(());
            }
        };
        match self.planner.decide(&state, &space, now) {
            Decision::Idle => {}
            Decision::Wait(at) => self.decide_at = Some(at),
            Decision::Cull(target) => {
                let stats = cull::run_pass(
                    self.dev,
                    &self.ctx,
                    &mut self.table,
                    target,
                    self.stop,
                    &self.wd,
                );
                self.counters.record_pass(&stats);
                if stats.kernel_eio > 0 {
                    self.check_health()?;
                }
                let now = Instant::now();
                let after = FreeSpace::of(&self.ctx.cache_root).unwrap_or(space);
                if let Some(wait) = self.planner.record(&after, &stats, now) {
                    self.decide_at = Some(now + wait);
                    if let Some(suppressed) = self.logs.no_progress.allow(now) {
                        warn!(
                            bavail = after.blocks_avail,
                            brun = state.brun,
                            candidates = stats.candidates,
                            skipped_busy = stats.skipped_busy,
                            errored = stats.errored,
                            set_aside = self.table.skipped(),
                            retry_in_s = wait.as_secs(),
                            suppressed,
                            "cull remains active but pass made no progress; backing off"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn timers(&mut self) -> Result<()> {
        let now = Instant::now();
        if now >= self.next_health {
            self.next_health = now + HEALTH_INTERVAL;
            self.check_health()?;
        }
        if now >= self.next_graveyard {
            self.next_graveyard = now + GRAVEYARD_INTERVAL;
            self.graveyard();
        }
        if let Some(at) = self.next_usage {
            if now >= at {
                let interval = Duration::from_secs(self.cfg.metrics.usage_scan_interval_secs);
                self.next_usage = Some(now + interval);
                self.scan_usage(interval);
            }
        }
        if self.metrics.as_ref().is_some_and(|m| m.due(now)) {
            self.write_metrics(now);
        }
        if now >= self.next_heartbeat {
            self.next_heartbeat = now + HEARTBEAT_INTERVAL;
            self.heartbeat();
        }
        Ok(())
    }

    /// Exit when the kernel has stopped the cache after an I/O error: it
    /// caches nothing more until the fd is closed and the cache bound
    /// again, which a restart by systemd does.
    fn check_health(&mut self) -> Result<()> {
        let text = match std::fs::read_to_string(self.caches_proc) {
            Ok(text) => text,
            Err(e) => {
                debug!(error = %e, path = self.caches_proc, "cannot read cache states");
                return Ok(());
            }
        };
        let state = cache_record_state(&text, &self.cfg.tag).map(str::to_string);
        self.record_state = state.clone();
        match state.as_deref() {
            Some("E") => {
                error!(
                    tag = self.cfg.tag,
                    "the kernel stopped the cache after an I/O error (state E in {CACHES_PROC}); exiting so that it is bound again"
                );
                return Err(Error::CacheDead {
                    tag: self.cfg.tag.clone(),
                });
            }
            Some(_) => {}
            None => {
                if let Some(suppressed) = self.logs.record.allow(Instant::now()) {
                    warn!(
                        tag = self.cfg.tag,
                        suppressed, "cache record not found in {CACHES_PROC}"
                    );
                }
            }
        }
        Ok(())
    }

    fn graveyard(&mut self) {
        let stats = cull::clean_graveyard(&self.ctx.cache_root, Some(self.stop), &self.wd);
        self.counters.record_graveyard(&stats);
        if stats.graveyard_removed > 0 || stats.errored > 0 {
            info!(
                removed = stats.graveyard_removed,
                bytes_freed = stats.bytes_freed,
                errored = stats.errored,
                "graveyard cleanup done"
            );
        }
    }

    fn scan_usage(&mut self, interval: Duration) {
        // A cull pass that walked the cache recently already knows.
        if self
            .table
            .usage()
            .is_some_and(|u| u.at.elapsed() < interval / 2)
        {
            return;
        }
        let started = Instant::now();
        if let Some(usage) = cull::cache_usage(&self.ctx.cache_root, self.stop, &self.wd) {
            debug!(
                objects = usage.objects,
                allocated = usage.allocated_bytes,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "cache usage scanned"
            );
            self.usage = Some(usage);
        }
    }

    fn current_usage(&self) -> Option<Usage> {
        match (self.usage, self.table.usage()) {
            (Some(a), Some(b)) => Some(if a.at >= b.at { a } else { b }),
            (a, b) => a.or(b),
        }
    }

    fn write_metrics(&mut self, now: Instant) {
        let space = FreeSpace::of(&self.ctx.cache_root).ok();
        let quota = self
            .cfg
            .max_size
            .and_then(|_| quota::usage(&self.ctx.cache_root, self.cfg.quota_project_id).ok());
        let fscache_stats = std::fs::read_to_string(STATS_PROC).ok();
        let snapshot = Snapshot {
            tag: &self.cfg.tag,
            state: self.last_state.as_ref(),
            space,
            cache_record: self.record_state.as_deref(),
            usage: self.current_usage(),
            quota,
            fscache_stats: fscache_stats.as_deref(),
        };
        if let Some(metrics) = &mut self.metrics {
            metrics.write(&snapshot, &self.counters, now);
        }
    }

    fn heartbeat(&self) {
        let Some(s) = self.last_state else {
            info!("heartbeat (no state read yet)");
            return;
        };
        // b*/f* are the kernel's limits, fixed at bind time; bavail/ffree
        // are the free counts it compares them to.
        let space = FreeSpace::of(&self.ctx.cache_root).ok();
        info!(
            culling = s.culling,
            bavail = space.map(|f| f.blocks_avail),
            bstop = s.bstop,
            bcull = s.bcull,
            brun = s.brun,
            ffree = space.map(|f| f.files_free),
            fstop = s.fstop,
            fcull = s.fcull,
            frun = s.frun,
            cache_state = self.record_state.as_deref().unwrap_or("?"),
            "heartbeat"
        );
    }
}

/// Pre-bind sanity checks. None of them are fatal — they emit `warn!`
/// when they spot something that will produce a confusing kernel error
/// later, or that silently degrades correctness.
fn preflight(tag: &str, cache_root: &Path) {
    check_tag_unique(tag);
    check_atime(cache_root);
}

/// Read /proc/fs/fscache/caches and warn if a live cache already uses
/// `tag`. The kernel itself rejects the bind then, but with an opaque
/// "kernel rejected command bind" error; we want a clearer hint.
fn check_tag_unique(tag: &str) {
    let text = match std::fs::read_to_string(CACHES_PROC) {
        Ok(t) => t,
        Err(e) => {
            // Module not loaded yet (nothing to conflict with) or proc
            // unavailable. Either way, don't fail; bind will tell us.
            debug!(error = %e, path = CACHES_PROC, "tag uniqueness pre-check skipped");
            return;
        }
    };
    if let Some(state) = tag_in_use(&text, tag) {
        warn!(
            tag,
            state,
            "tag in use by a cache in /proc/fs/fscache/caches; bind will likely fail with EBUSY"
        );
    }
}

/// State of the cache record named `tag`. Format (kernel
/// fs/netfs/fscache_cache.c):
///
/// ```text
/// CACHE    REF   VOLS  OBJS  ACCES S NAME
/// ======== ===== ===== ===== ===== = ===============
/// 00000001     2     1    20     1 A nfscache
/// ```
///
/// States: `-` no cache bound, `P` preparing, `A` active, `E` stopped by
/// an I/O error, `W` withdrawing.
fn cache_record_state<'a>(caches: &'a str, tag: &str) -> Option<&'a str> {
    caches
        .lines()
        .filter(|line| !line.starts_with('=') && !line.starts_with("CACHE"))
        .find_map(|line| {
            let mut cols = line.split_ascii_whitespace().rev();
            let name = cols.next()?;
            let state = cols.next()?;
            (name == tag).then_some(state)
        })
}

/// State of the cache record named `tag`, if that record would make `bind`
/// fail. The record outlives its daemon while an `fsc` mount still
/// references it, in state `-` (no cache bound), and the next bind with the
/// same tag takes it over — that is every daemon restart. Only the other
/// states mean the tag is taken.
fn tag_in_use<'a>(caches: &'a str, tag: &str) -> Option<&'a str> {
    cache_record_state(caches, tag).filter(|state| *state != "-")
}

/// atime is the LRU key the cull algorithm relies on. Warn if the cache
/// mount has it frozen (`noatime`), and note the granularity `relatime`
/// gives it.
fn check_atime(cache_root: &Path) {
    let text = match std::fs::read_to_string("/proc/self/mountinfo") {
        Ok(t) => t,
        Err(e) => {
            debug!(error = %e, "could not read /proc/self/mountinfo for atime check");
            return;
        }
    };
    let Some(opts) = mount_options(&text, cache_root) else {
        return;
    };
    if opts.split(',').any(|o| o == "noatime") {
        warn!(
            cache_dir = %cache_root.display(),
            "cache_dir filesystem mounted noatime; atime is frozen and cull LRU degrades to insertion order. Remount with relatime (default) to restore LRU semantics."
        );
    } else if !opts.split(',').any(|o| o == "strictatime") {
        info!(
            cache_dir = %cache_root.display(),
            "cache_dir mounted relatime: an object's atime moves when it is opened, at most once a day, so cull order is by day of last use"
        );
    }
}

/// Mount options (mountinfo field 6) of the mount holding `path`: the one
/// whose mount point is the longest prefix of the canonical path.
fn mount_options(mountinfo: &str, path: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut best: Option<(usize, &str)> = None;
    for line in mountinfo.lines() {
        let mut iter = line.split_ascii_whitespace().skip(4);
        let Some(mp) = iter.next() else { continue };
        let Some(opts) = iter.next() else { continue };
        if canonical.starts_with(mp) && best.map_or(true, |(len, _)| mp.len() > len) {
            best = Some((mp.len(), opts));
        }
    }
    best.map(|(_, opts)| opts.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::fs::FileTimes;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;
    use std::time::SystemTime;

    const CACHES: &str = "CACHE    REF   VOLS  OBJS  ACCES S NAME\n\
                          ======== ===== ===== ===== ===== = ===============\n\
                          00000001     2     1    20     1 A nfscache\n";

    #[test]
    fn tag_in_use_by_active_cache() {
        assert_eq!(tag_in_use(CACHES, "nfscache"), Some("A"));
        assert_eq!(tag_in_use(CACHES, "other"), None);
    }

    #[test]
    fn tag_free_while_record_has_no_cache_bound() {
        // Regression: every restart warned "bind will likely fail" because
        // the record left behind by the previous daemon (state `-`, kept
        // alive by the fsc mount) matched; the bind then succeeded.
        let after_exit = CACHES.replace(" A nfscache", " - nfscache");
        assert_eq!(tag_in_use(&after_exit, "nfscache"), None);
    }

    #[test]
    fn tag_in_use_ignores_header() {
        assert_eq!(tag_in_use(CACHES, "NAME"), None);
        assert_eq!(tag_in_use("", "nfscache"), None);
    }

    #[test]
    fn cache_record_state_spots_an_io_error() {
        let dead = CACHES.replace(" A nfscache", " E nfscache");
        assert_eq!(cache_record_state(&dead, "nfscache"), Some("E"));
        assert_eq!(cache_record_state(CACHES, "nfscache"), Some("A"));
        assert_eq!(cache_record_state(CACHES, "other"), None);
    }

    #[test]
    fn mount_options_pick_the_longest_prefix() {
        let mountinfo = "\
22 1 259:1 / / rw,relatime shared:1 - ext4 /dev/root rw\n\
30 22 8:17 / /srv/data rw,noatime shared:2 - xfs /dev/sdb1 rw,prjquota\n\
31 30 8:17 /cache /srv/data/cache rw,relatime shared:3 - xfs /dev/sdb1 rw,prjquota\n";
        assert_eq!(
            mount_options(mountinfo, Path::new("/srv/data/cache")).as_deref(),
            Some("rw,relatime")
        );
        assert_eq!(
            mount_options(mountinfo, Path::new("/srv/data/other")).as_deref(),
            Some("rw,noatime")
        );
        assert_eq!(
            mount_options(mountinfo, Path::new("/srv")).as_deref(),
            Some("rw,relatime")
        );
    }

    /// Stands in for cachefiles: `cull <name>` unlinks `name` relative to the
    /// working directory (the kernel uses the daemon's), the cull flag
    /// follows statfs of the cache directory with the kernel's rules, and
    /// the poll descriptor is always readable. Sets `stop` a few reads after
    /// the flag cleared.
    struct FakeKernel {
        event: OwnedFd,
        root: PathBuf,
        brun: u64,
        bcull: u64,
        busy: Vec<&'static str>,
        culling: Cell<bool>,
        culled: RefCell<Vec<String>>,
        quiet_reads: Cell<u32>,
        stop: &'static AtomicBool,
    }

    impl Kernel for FakeKernel {
        fn as_raw_fd(&self) -> RawFd {
            self.event.as_raw_fd()
        }

        fn write_cmd(&self, cmd: &str) -> Result<()> {
            let name = cmd
                .strip_prefix("cull ")
                .expect("only cull commands after bind");
            let kernel_error = |source| Error::Kernel {
                cmd: cmd.to_string(),
                source,
            };
            if self.busy.contains(&name) {
                return Err(kernel_error(std::io::Error::from_raw_os_error(libc::EBUSY)));
            }
            std::fs::remove_file(name).map_err(kernel_error)?;
            self.culled.borrow_mut().push(name.to_string());
            Ok(())
        }

        fn read_state(&self, buf: &mut [u8]) -> Result<usize> {
            let avail = FreeSpace::of(&self.root)?.blocks_avail;
            if avail < self.bcull {
                self.culling.set(true);
            } else if self.culling.get() && avail >= self.brun {
                self.culling.set(false);
            }
            if !self.culling.get() && !self.culled.borrow().is_empty() {
                self.quiet_reads.set(self.quiet_reads.get() + 1);
                if self.quiet_reads.get() >= 3 {
                    self.stop.store(true, Ordering::Relaxed);
                }
            }
            let line = format!(
                "cull={} frun=0 fcull=0 fstop=0 brun={:x} bcull={:x} bstop=0",
                u8::from(self.culling.get()),
                self.brun,
                self.bcull
            );
            buf[..line.len()].copy_from_slice(line.as_bytes());
            Ok(line.len())
        }
    }

    #[test]
    fn runner_culls_one_episode_end_to_end() {
        static STOP: AtomicBool = AtomicBool::new(false);
        let root = std::env::temp_dir().join(format!(
            "nfs-cachefs-runner-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bucket = root.join("cache").join("Ivol").join("@00");
        std::fs::create_dir_all(&bucket).unwrap();
        let mut sizes = Vec::new();
        for i in 0..10u64 {
            let path = bucket.join(format!("D{i:02}"));
            std::fs::write(&path, vec![b'x'; 4 << 20]).unwrap();
            let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + i);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(FileTimes::new().set_accessed(at).set_modified(at))
                .unwrap();
            sizes.push(std::fs::metadata(&path).unwrap().blocks() * 512);
        }

        // D00, the oldest, is in use. Getting back to run takes five and a
        // half of the next objects: six culls. The half object absorbs
        // whatever else writes to this filesystem meanwhile.
        let space = FreeSpace::of(&root).unwrap();
        let need = (sizes[1..6].iter().sum::<u64>() + sizes[6] / 2) / space.block_size;
        let brun = space.blocks_avail + need;
        // A 10-block band makes the planner's margin one block.
        let bcull = brun - 10;

        let cfg: Config = toml::from_str(&format!(
            "cache_dir = {:?}\ntag = \"t\"\n[metrics]\ntextfile = {:?}\ninterval_secs = 60\n",
            root.display().to_string(),
            root.join("nfs-cachefs.prom").display().to_string(),
        ))
        .unwrap();
        // SAFETY: eventfd returns a fresh descriptor or -1.
        let event = unsafe {
            let fd = libc::eventfd(1, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC);
            assert!(fd >= 0);
            OwnedFd::from_raw_fd(fd)
        };
        let fake = FakeKernel {
            event,
            root: root.clone(),
            brun,
            bcull,
            busy: vec!["D00"],
            culling: Cell::new(false),
            culled: RefCell::new(Vec::new()),
            quiet_reads: Cell::new(0),
            stop: &STOP,
        };

        Runner::new(&cfg, &fake, &STOP).run().unwrap();

        assert_eq!(
            fake.culled.borrow().as_slice(),
            ["D01", "D02", "D03", "D04", "D05", "D06"]
        );
        let mut left: Vec<_> = std::fs::read_dir(&bucket)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort_unstable();
        assert_eq!(left, ["D00", "D07", "D08", "D09"]);
        let metrics = std::fs::read_to_string(root.join("nfs-cachefs.prom")).unwrap();
        assert!(
            metrics.contains("nfs_cachefs_culled_objects_total 6"),
            "{metrics}"
        );
        assert!(
            metrics.contains("nfs_cachefs_cull_busy_skips_total 1"),
            "{metrics}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn runner_exits_when_the_kernel_stops_the_cache() {
        static STOP: AtomicBool = AtomicBool::new(false);
        let root = std::env::temp_dir().join(format!(
            "nfs-cachefs-dead-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("cache")).unwrap();
        let caches = root.join("caches");
        std::fs::write(&caches, CACHES.replace(" A nfscache", " E t")).unwrap();
        let cfg: Config = toml::from_str(&format!(
            "cache_dir = {:?}\ntag = \"t\"\n[metrics]\ntextfile = {:?}\n",
            root.display().to_string(),
            root.join("nfs-cachefs.prom").display().to_string(),
        ))
        .unwrap();
        // SAFETY: eventfd returns a fresh descriptor or -1.
        let event = unsafe {
            let fd = libc::eventfd(1, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC);
            assert!(fd >= 0);
            OwnedFd::from_raw_fd(fd)
        };
        let space = FreeSpace::of(&root).unwrap();
        let fake = FakeKernel {
            event,
            root: root.clone(),
            brun: space.blocks_avail / 2,
            bcull: space.blocks_avail / 4,
            busy: Vec::new(),
            culling: Cell::new(false),
            culled: RefCell::new(Vec::new()),
            quiet_reads: Cell::new(0),
            stop: &STOP,
        };
        let mut runner = Runner::new(&cfg, &fake, &STOP);
        runner.caches_proc = Box::leak(caches.display().to_string().into_boxed_str());

        assert!(matches!(runner.run(), Err(Error::CacheDead { .. })));
        runner.write_metrics(Instant::now());
        let metrics = std::fs::read_to_string(root.join("nfs-cachefs.prom")).unwrap();
        assert!(
            metrics.contains("nfs_cachefs_cache_io_error 1"),
            "{metrics}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
