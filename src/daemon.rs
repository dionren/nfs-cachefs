//! Main event loop. Polls `/dev/cachefiles`; on POLLIN, reads the kernel's
//! state line and triggers a cull pass when needed.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use crate::cull::{self, CullCtx};
use crate::error::{Error, Result};
use crate::proto::{CacheState, ConfigCmd, Device};

/// How often to log a heartbeat / metrics summary at INFO when idle.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// Poll timeout. The kernel marks the device readable on state changes,
/// but a wakeup also lets us run the heartbeat and check the stop flag.
const POLL_TIMEOUT_MS: i32 = 5_000;

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

        let mut buf = [0u8; 256];
        let mut last_heartbeat = Instant::now();
        let mut last_state: Option<CacheState> = None;

        while !self.stop.load(Ordering::Relaxed) {
            let mut pollfd = libc::pollfd {
                fd: self.dev.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let r = unsafe { libc::poll(&mut pollfd, 1, POLL_TIMEOUT_MS) };
            if r < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(Error::Io(err));
            }

            if r > 0 && (pollfd.revents & libc::POLLIN) != 0 {
                match self.dev.read_state(&mut buf) {
                    Ok(0) => {} // not ready yet
                    Ok(n) => match CacheState::parse(&buf[..n]) {
                        Ok(state) => {
                            debug!(?state, "state");
                            if state.culling {
                                cull::run_pass(&self.dev, &self.cull);
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

            if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                last_heartbeat = Instant::now();
                if let Some(s) = last_state {
                    info!(
                        culling = s.culling,
                        bstop = s.bstop, bcull = s.bcull, brun = s.brun,
                        fstop = s.fstop, fcull = s.fcull, frun = s.frun,
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
}

/// Pre-bind sanity checks. None of them are fatal — they emit `warn!`
/// when they spot something that will produce a confusing kernel error
/// later, or that silently degrades correctness.
fn preflight(tag: &str, cache_root: &Path) {
    check_tag_unique(tag);
    check_atime_enabled(cache_root);
}

/// Read /proc/fs/fscache/caches and warn if `tag` is already listed.
/// The kernel itself rejects duplicate tags at bind time, but with an
/// opaque "kernel rejected command bind" error; we want a clearer hint.
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
    for line in text.lines() {
        // Format (kernel fs/fscache/cache.c):
        //   "Cache         State    Tag\n"
        //   "==========    ===      ===\n"
        //   "<name>        <state>  <tag>\n"
        // We don't anchor; an exact-token match anywhere on a non-header
        // line is enough to flag a collision.
        if line.starts_with('=') || line.starts_with("Cache") {
            continue;
        }
        if line.split_ascii_whitespace().any(|t| t == tag) {
            warn!(
                tag,
                "tag already present in /proc/fs/fscache/caches; bind will likely fail with EBUSY"
            );
            return;
        }
    }
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
}
