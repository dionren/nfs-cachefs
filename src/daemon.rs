//! Main event loop. Polls `/dev/cachefiles`; on POLLIN, reads the kernel's
//! state line and triggers a cull pass when needed.

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
                                if let Err(e) = cull::run_pass(&self.dev, &self.cull) {
                                    warn!(error = %e, "cull pass error");
                                }
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
