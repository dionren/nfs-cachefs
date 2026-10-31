//! Minimal sd_notify(3) client: readiness, status, start-timeout extension
//! and the service watchdog. Keeping this local avoids pulling in
//! libsystemd or an extra crate for a few datagrams.

use std::cell::Cell;
use std::io;
use std::time::{Duration, Instant};

/// Send one notification. `Ok(false)` when not running under a systemd
/// notify socket.
#[cfg(target_os = "linux")]
pub fn notify(message: &str) -> io::Result<bool> {
    use std::env;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    use std::path::Path;

    let Some(socket) = env::var_os("NOTIFY_SOCKET") else {
        return Ok(false);
    };
    if socket.is_empty() {
        return Ok(false);
    }

    let bytes = socket.as_bytes();
    let addr = if bytes.first() == Some(&b'@') {
        SocketAddr::from_abstract_name(&bytes[1..])?
    } else {
        SocketAddr::from_pathname(Path::new(&socket))?
    };

    let sock = UnixDatagram::unbound()?;
    sock.send_to_addr(message.as_bytes(), &addr)?;
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
pub fn notify(_message: &str) -> io::Result<bool> {
    Ok(false)
}

/// Tell systemd the service is up (`Type=notify`).
pub fn ready(status: &str) -> io::Result<bool> {
    notify(&format!("READY=1\nSTATUS={status}"))
}

/// Update the status line `systemctl status` shows. Best effort.
pub fn status(status: &str) {
    let _ = notify(&format!("STATUS={status}"));
}

/// Tell systemd the service is shutting down. Best effort.
pub fn stopping() {
    let _ = notify("STOPPING=1");
}

/// Ask systemd for `extra` more time to finish starting up, counted from
/// now. Used while a long migration runs before the cache is bound.
pub fn extend_start_timeout(extra: Duration, status: &str) {
    let _ = notify(&format!(
        "EXTEND_TIMEOUT_USEC={}\nSTATUS={status}",
        extra.as_micros()
    ));
}

/// The service watchdog (`WatchdogSec=`). [`Watchdog::kick`] is cheap
/// enough to call once per object in long loops: it sends a datagram only
/// when half the interval has passed since the last one.
#[derive(Debug)]
pub struct Watchdog {
    interval: Option<Duration>,
    last: Cell<Option<Instant>>,
}

impl Watchdog {
    /// The watchdog systemd configured for this process, if any.
    pub fn from_env() -> Self {
        let interval = std::env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|usec| usec.parse::<u64>().ok())
            .filter(|&usec| usec > 0)
            .filter(|_| watchdog_pid_is_us())
            .map(Duration::from_micros);
        Self {
            interval,
            last: Cell::new(None),
        }
    }

    /// A watchdog that never sends anything.
    pub fn disabled() -> Self {
        Self {
            interval: None,
            last: Cell::new(None),
        }
    }

    pub fn interval(&self) -> Option<Duration> {
        self.interval
    }

    pub fn kick(&self) {
        let Some(interval) = self.interval else {
            return;
        };
        let now = Instant::now();
        let due = self
            .last
            .get()
            .map_or(true, |last| now.duration_since(last) >= interval / 2);
        if due {
            self.last.set(Some(now));
            if let Err(e) = notify("WATCHDOG=1") {
                tracing::debug!(error = %e, "watchdog notification failed");
            }
        }
    }
}

/// `WATCHDOG_PID`, when set, names the process the watchdog is meant for.
fn watchdog_pid_is_us() -> bool {
    match std::env::var("WATCHDOG_PID") {
        Ok(pid) => pid.parse::<u32>().ok() == Some(std::process::id()),
        Err(_) => true,
    }
}
