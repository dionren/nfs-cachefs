//! Small helpers shared by the daemon modules.

use std::time::{Duration, Instant};

/// Lets a repeating log message through at most once per `interval` and
/// counts how many were held back in between.
#[derive(Debug)]
pub(crate) struct RateLimit {
    interval: Duration,
    last: Option<Instant>,
    suppressed: u64,
}

impl RateLimit {
    pub(crate) const fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
            suppressed: 0,
        }
    }

    /// `Some(n)` when the message may be logged now, `n` being how many
    /// were suppressed since the last one that was.
    pub(crate) fn allow(&mut self, now: Instant) -> Option<u64> {
        match self.last {
            Some(last) if now.duration_since(last) < self.interval => {
                self.suppressed += 1;
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.suppressed))
            }
        }
    }
}

/// `bytes` with a binary unit, for log messages: "1.5 GiB".
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_counts_suppressed_messages() {
        let mut rl = RateLimit::new(Duration::from_secs(60));
        let t0 = Instant::now();
        assert_eq!(rl.allow(t0), Some(0));
        assert_eq!(rl.allow(t0 + Duration::from_secs(1)), None);
        assert_eq!(rl.allow(t0 + Duration::from_secs(2)), None);
        assert_eq!(rl.allow(t0 + Duration::from_secs(60)), Some(2));
    }

    #[test]
    fn human_bytes_picks_a_unit() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(3 << 40), "3.0 TiB");
    }
}
