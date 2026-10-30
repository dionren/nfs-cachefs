//! Main event loop. Polls `/dev/cachefiles`; on POLLIN/POLLOUT and on every
//! poll timeout, reads the kernel's state line and, when it asks for
//! culling, runs a pass sized to what the kernel needs freed, then waits
//! for the freed space to show up in statfs before considering another pass.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use crate::cull::{self, CullCtx, CullStats, CullTarget, FreeSpace};
use crate::error::{Error, Result};
use crate::proto::{CacheState, ConfigCmd, Device};

/// How often to log a heartbeat / metrics summary at INFO when idle.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// How often to drain cachefiles' graveyard even when the kernel is not asking
/// for active culling.
const GRAVEYARD_INTERVAL: Duration = Duration::from_secs(30);

/// Poll timeout. The kernel marks the device readable on state changes,
/// but a wakeup also lets us re-read the state, run the heartbeat and check
/// the stop flag.
const POLL_TIMEOUT_MS: i32 = 5_000;

/// Backoff when the kernel says culling is active but a pass cannot free
/// anything. This prevents a POLLOUT-driven busy loop when all candidates are
/// busy or the cache layout is not cullable by this daemon version.
const NO_PROGRESS_BACKOFF: Duration = Duration::from_secs(1);

/// Longest wait after a pass for the space it freed to show up in statfs.
/// XFS reclaims unlinked inodes in background workers, so right after a
/// burst of culls the kernel still sees the old free count and keeps
/// `cull=1`. Sizing another pass on those numbers would cull the same
/// shortfall again.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to re-check statfs while waiting for freed space to show up.
const SETTLE_CHECK_MS: i32 = 500;

pub struct Daemon<'a> {
    pub dev: Device,
    pub config: ConfigCmd<'a>,
    pub cull: CullCtx,
    pub stop: &'static AtomicBool,
}

impl<'a> Daemon<'a> {
    /// Bind the cache and run until `stop` is set. Returns on graceful exit
    /// or fatal error.
    pub fn run(self) -> Result<()> {
        preflight(self.config.tag, &self.cull.cache_root);

        self.config.apply_and_bind(&self.dev)?;
        info!(
            tag = self.config.tag,
            cache_dir = %self.config.cache_dir.display(),
            "cache bound (traditional mode)"
        );
        match crate::systemd_notify::ready("cache bound") {
            Ok(true) => debug!("sent systemd readiness notification"),
            Ok(false) => debug!("NOTIFY_SOCKET not set; readiness notification skipped"),
            Err(e) => {
                return Err(Error::Io(std::io::Error::other(format!(
                    "failed to notify systemd readiness: {e}"
                ))));
            }
        }

        let mut buf = [0u8; 256];
        let mut last_heartbeat = Instant::now();
        let mut last_graveyard = Instant::now();
        let mut last_state: Option<CacheState> = None;
        let mut settle: Option<Settle> = None;
        log_graveyard_cleanup(cull::clean_graveyard(&self.cull.cache_root));

        while !self.stop.load(Ordering::Relaxed) {
            // The kernel asserts POLLOUT for as long as its cull flag is set.
            // While settling, wake only for state changes and the statfs
            // re-check, or poll would spin until the freed space shows up.
            let (events, timeout_ms) = match &settle {
                Some(s) => (libc::POLLIN, s.poll_timeout_ms(Instant::now())),
                None => (libc::POLLIN | libc::POLLOUT, POLL_TIMEOUT_MS),
            };
            let mut pollfd = libc::pollfd {
                fd: self.dev.as_raw_fd(),
                events,
                revents: 0,
            };
            let r = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
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
            let signalled = r > 0 && (pollfd.revents & (libc::POLLIN | libc::POLLOUT)) != 0;
            if signalled || r == 0 {
                match self.dev.read_state(&mut buf) {
                    Ok(0) => {} // not ready yet
                    Ok(n) => match CacheState::parse(&buf[..n]) {
                        Ok(state) => {
                            debug!(?state, "state");
                            if !state.culling {
                                settle = None;
                            } else if settle.is_none() {
                                settle = self.cull_pass(&state);
                            }
                            last_state = Some(state);
                        }
                        Err(e) => warn!(error = %e, "failed to parse state line"),
                    },
                    Err(Error::Io(e)) if e.raw_os_error() == Some(libc::EINTR) => {}
                    Err(e) => {
                        error!(error = %e, "read /dev/cachefiles failed");
                        return Err(e);
                    }
                }
            }

            if let Some(s) = &settle {
                let space = FreeSpace::of(&self.cull.cache_root).ok();
                if let Some(reason) = s.over(space.as_ref(), Instant::now()) {
                    debug!(
                        reason,
                        bavail = space.map(|f| f.blocks_avail),
                        "cull settle over; re-evaluating"
                    );
                    settle = None;
                }
            }

            if last_graveyard.elapsed() >= GRAVEYARD_INTERVAL {
                last_graveyard = Instant::now();
                log_graveyard_cleanup(cull::clean_graveyard(&self.cull.cache_root));
            }

            if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                last_heartbeat = Instant::now();
                if let Some(s) = last_state {
                    // b*/f* are the kernel's limits, fixed at bind time;
                    // bavail/ffree are the free counts it compares them to.
                    let space = FreeSpace::of(&self.cull.cache_root).ok();
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
                        "heartbeat"
                    );
                } else {
                    info!("heartbeat (no state read yet)");
                }
            }
        }

        info!("stop signal received; closing /dev/cachefiles (kernel will unbind)");
        // self.dev drops here, closing the fd; the kernel withdraws the cache.
        Ok(())
    }

    /// Cull what the kernel needs freed to clear `cull=1`. Returns the
    /// settle wait to observe before the next pass, or `None` after a short
    /// backoff when nothing was freed.
    fn cull_pass(&self, state: &CacheState) -> Option<Settle> {
        let space = match FreeSpace::of(&self.cull.cache_root) {
            Ok(space) => space,
            Err(e) => {
                warn!(error = %e, "statfs on cache_dir failed; cannot size cull pass, backing off");
                sleep_with_stop(self.stop, NO_PROGRESS_BACKOFF);
                return None;
            }
        };
        let target = CullTarget::new(state, &space);
        if target.is_zero() {
            // The kernel counts in-flight cache writes as used, so it can
            // still want culling when statfs already shows `run` reached.
            // It re-evaluates on every read; look again shortly.
            debug!(
                bavail = space.blocks_avail,
                brun = state.brun,
                ffree = space.files_free,
                frun = state.frun,
                "cull requested but free space is already at run; re-checking"
            );
            sleep_with_stop(self.stop, NO_PROGRESS_BACKOFF);
            return None;
        }

        let stats = cull::run_pass(&self.dev, &self.cull, target, self.stop);
        if !stats.made_progress() {
            warn!(
                candidates = stats.candidates,
                skipped_busy = stats.skipped_busy,
                errored = stats.errored,
                "cull remains active but pass made no progress; backing off"
            );
            sleep_with_stop(self.stop, NO_PROGRESS_BACKOFF);
            return None;
        }
        Some(Settle::new(state, &space, &stats, Instant::now()))
    }
}

/// Wait after a cull pass: the next pass is sized only once statfs shows
/// what this one freed, so it is not sized on stale numbers.
struct Settle {
    deadline: Instant,
    /// Free blocks / inodes once the freed space is visible, capped at
    /// `brun` / `frun`: reaching those is all the kernel needs.
    want_blocks: u64,
    want_files: u64,
    /// `bstop` / `fstop`. Below these the kernel has stopped caching new
    /// data; waiting any longer only prolongs that.
    floor_blocks: u64,
    floor_files: u64,
}

impl Settle {
    fn new(state: &CacheState, before: &FreeSpace, freed: &CullStats, now: Instant) -> Self {
        Self {
            deadline: now + SETTLE_TIMEOUT,
            want_blocks: before
                .blocks_avail
                .saturating_add(freed.bytes_freed / before.block_size)
                .min(state.brun),
            want_files: before
                .files_free
                .saturating_add(freed.files_freed)
                .min(state.frun),
            floor_blocks: state.bstop,
            floor_files: state.fstop,
        }
    }

    /// Why the wait is over, if it is. `space` is the current statfs, when
    /// it could be read.
    fn over(&self, space: Option<&FreeSpace>, now: Instant) -> Option<&'static str> {
        if let Some(space) = space {
            if space.blocks_avail >= self.want_blocks && space.files_free >= self.want_files {
                return Some("freed space visible");
            }
            if space.blocks_avail < self.floor_blocks || space.files_free < self.floor_files {
                return Some("below stop limits");
            }
        }
        (now >= self.deadline).then_some("timed out")
    }

    fn poll_timeout_ms(&self, now: Instant) -> i32 {
        let left = self.deadline.saturating_duration_since(now).as_millis();
        i32::try_from(left).map_or(SETTLE_CHECK_MS, |ms| ms.min(SETTLE_CHECK_MS))
    }
}

fn log_graveyard_cleanup(stats: cull::CullStats) {
    if stats.graveyard_removed > 0 || stats.errored > 0 {
        info!(
            removed = stats.graveyard_removed,
            bytes_freed = stats.bytes_freed,
            errored = stats.errored,
            "graveyard cleanup done"
        );
    }
}

fn sleep_with_stop(stop: &AtomicBool, duration: Duration) {
    let started = Instant::now();
    while !stop.load(Ordering::Relaxed) && started.elapsed() < duration {
        let remaining = duration.saturating_sub(started.elapsed());
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

/// Pre-bind sanity checks. None of them are fatal — they emit `warn!`
/// when they spot something that will produce a confusing kernel error
/// later, or that silently degrades correctness.
fn preflight(tag: &str, cache_root: &Path) {
    check_tag_unique(tag);
    check_atime_enabled(cache_root);
}

/// Read /proc/fs/fscache/caches and warn if a live cache already uses
/// `tag`. The kernel itself rejects the bind then, but with an opaque
/// "kernel rejected command bind" error; we want a clearer hint.
fn check_tag_unique(tag: &str) {
    let path = "/proc/fs/fscache/caches";
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            // Module not loaded yet (nothing to conflict with) or proc
            // unavailable. Either way, don't fail; bind will tell us.
            debug!(error = %e, path, "tag uniqueness pre-check skipped");
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

/// State of the cache record named `tag`, if that record would make `bind`
/// fail. Format (kernel fs/fscache/cache.c):
///
/// ```text
/// CACHE    REF   VOLS  OBJS  ACCES S NAME
/// ======== ===== ===== ===== ===== = ===============
/// 00000001     2     1    20     1 A nfscache
/// ```
///
/// The record outlives its daemon while an `fsc` mount still references
/// it, in state `-` (no cache bound), and the next bind with the same tag
/// takes it over — that is every daemon restart. Only the other states
/// (`P`reparing, `A`ctive, `E` I/O error, `W`ithdrawing) mean the tag is
/// taken.
fn tag_in_use<'a>(caches: &'a str, tag: &str) -> Option<&'a str> {
    caches
        .lines()
        .filter(|line| !line.starts_with('=') && !line.starts_with("CACHE"))
        .find_map(|line| {
            let mut cols = line.split_ascii_whitespace().rev();
            let name = cols.next()?;
            let state = cols.next()?;
            (name == tag && state != "-").then_some(state)
        })
}

/// Warn if `cache_root` is on a filesystem mounted with `noatime`. atime
/// is the LRU key the cull algorithm relies on; `noatime` freezes it on
/// read and silently degrades cull to insertion-order eviction.
fn check_atime_enabled(cache_root: &Path) {
    let text = match std::fs::read_to_string("/proc/self/mountinfo") {
        Ok(t) => t,
        Err(e) => {
            debug!(error = %e, "could not read /proc/self/mountinfo for atime check");
            return;
        }
    };
    let canonical = std::fs::canonicalize(cache_root).unwrap_or_else(|_| cache_root.to_path_buf());

    // Walk all mountinfo lines and pick the one whose mount point is the
    // longest prefix of the canonical cache_root. Per-mount options are
    // in field 6 (1-indexed: id parent dev root mount-point options ...).
    let mut best: Option<(usize, String)> = None;
    for line in text.lines() {
        let mut iter = line.split_ascii_whitespace().skip(4);
        let Some(mp) = iter.next() else { continue };
        let Some(opts) = iter.next() else { continue };
        if canonical.starts_with(mp) {
            let len = mp.len();
            if best.as_ref().map_or(true, |(blen, _)| len > *blen) {
                best = Some((len, opts.to_string()));
            }
        }
    }
    if let Some((_, opts)) = best {
        if opts.split(',').any(|o| o == "noatime") {
            warn!(
                cache_dir = %cache_root.display(),
                "cache_dir filesystem mounted noatime; atime is frozen and cull LRU degrades to insertion order. Remount with relatime (default) to restore LRU semantics."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    // check_tag_unique and check_atime_enabled are intentionally
    // non-erroring (they only emit warn!) so tests would just be
    // observational. The procfs paths they read are not portable to
    // CI sandboxes either; we cover the broader behavior in the e2e
    // script and rely on integration testing on the test machine.
    // The /proc/fs/fscache/caches parsing is tested via tag_in_use.

    use super::*;

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

    const STATE: CacheState = CacheState {
        culling: true,
        frun: 100,
        fcull: 70,
        fstop: 30,
        brun: 1000,
        bcull: 700,
        bstop: 300,
    };

    fn space(blocks_avail: u64, files_free: u64) -> FreeSpace {
        FreeSpace {
            block_size: 4096,
            blocks_avail,
            files_free,
        }
    }

    fn freed(blocks: u64) -> CullStats {
        CullStats {
            culled: 1,
            bytes_freed: blocks * 4096,
            files_freed: 1,
            ..CullStats::default()
        }
    }

    #[test]
    fn settle_waits_for_freed_space_to_show_up() {
        // Regression: statfs lags behind unlinks on XFS. Sizing the next
        // pass before it catches up culls the same shortfall again.
        let now = Instant::now();
        let s = Settle::new(&STATE, &space(650, 500), &freed(200), now);
        assert_eq!(s.over(Some(&space(650, 500)), now), None);
        assert_eq!(s.over(Some(&space(849, 500)), now), None);
        assert_eq!(
            s.over(Some(&space(850, 500)), now),
            Some("freed space visible")
        );
    }

    #[test]
    fn settle_needs_no_more_than_run() {
        let now = Instant::now();
        let s = Settle::new(&STATE, &space(650, 500), &freed(5_000), now);
        assert_eq!(
            s.over(Some(&space(1000, 500)), now),
            Some("freed space visible")
        );
    }

    #[test]
    fn settle_gives_up_below_stop_or_after_timeout() {
        let now = Instant::now();
        let s = Settle::new(&STATE, &space(650, 500), &freed(200), now);
        assert_eq!(
            s.over(Some(&space(299, 500)), now),
            Some("below stop limits")
        );
        assert_eq!(s.over(None, now), None);
        assert_eq!(s.over(None, now + SETTLE_TIMEOUT), Some("timed out"));
    }

    #[test]
    fn settle_poll_timeout_never_passes_deadline() {
        let now = Instant::now();
        let s = Settle::new(&STATE, &space(650, 500), &freed(200), now);
        assert_eq!(s.poll_timeout_ms(now), SETTLE_CHECK_MS);
        let near_end = now + SETTLE_TIMEOUT - Duration::from_millis(100);
        assert_eq!(s.poll_timeout_ms(near_end), 100);
        assert_eq!(s.poll_timeout_ms(now + SETTLE_TIMEOUT * 2), 0);
    }
}
