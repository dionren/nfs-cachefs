//! When to cull and how much. The decisions behind the daemon loop, kept
//! free of I/O so they can be tested against a simulated filesystem.
//!
//! ## Sizing
//!
//! A pass is sized on statfs right before it: the blocks and inodes missing
//! to bring free space back above `brun` / `frun`. A pass frees most of
//! what it culls on the spot (it truncates culled files through a held
//! descriptor, see `cull.rs`), so statfs right after it is accurate.
//!
//! ## Freed but not yet visible
//!
//! What a pass could not free on the spot (`bytes_async`: directories, a
//! failed truncate) the filesystem releases in the background, and XFS
//! can take seconds per heavily fragmented file. Until then statfs — and
//! the kernel's `cull=1` — count it as used, and sizing the next pass on
//! those numbers would cull it a second time. The planner remembers such
//! space and subtracts what statfs does not show yet ("pending") from the
//! next target. It counts as shown once statfs has gained it, or after
//! [`CONFIRM_WINDOW`] at the latest, so that space other writes take in the
//! meantime is still given back.
//!
//! ## Margin
//!
//! The kernel clears `cull=1` once free space minus the blocks of cache
//! writes in flight reaches `brun`. Culling to exactly `brun` would leave
//! the flag set under steady writes and invite a string of tiny passes, so
//! the block target carries a small margin above `brun`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::cull::{CullStats, CullTarget, FreeSpace};
use crate::proto::CacheState;

/// How long space freed in the background may take to show up in statfs
/// before the planner assumes it has (and that whatever is still missing
/// was taken by other writes).
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(60);

/// How soon to look again when culling is active but nothing needs to be
/// culled right now (pending frees cover the shortfall, or in-flight writes
/// keep the kernel's flag set).
pub const RECHECK: Duration = Duration::from_secs(1);

/// Backoff after a pass that freed nothing, doubling up to the maximum.
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Upper bound of the margin above `brun`. It is also capped at a tenth of
/// the `bcull`..`brun` band.
pub const MARGIN_BYTES: u64 = 1 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The kernel does not want culling.
    Idle,
    /// Culling is active; decide again at this time.
    Wait(Instant),
    /// Run a pass now.
    Cull(CullTarget),
}

#[derive(Debug, Clone, Copy)]
struct Pass {
    at: Instant,
    /// statfs right after the pass.
    after_blocks: u64,
    after_files: u64,
    /// Freed in the background, not yet in `after_*`.
    blocks: u64,
    files: u64,
}

#[derive(Debug)]
pub struct Planner {
    recent: VecDeque<Pass>,
    backoff: Duration,
    retry_at: Option<Instant>,
}

impl Default for Planner {
    fn default() -> Self {
        Self {
            recent: VecDeque::new(),
            backoff: BACKOFF_MIN,
            retry_at: None,
        }
    }
}

impl Planner {
    /// The kernel stopped asking for culling.
    pub fn idle(&mut self) {
        self.backoff = BACKOFF_MIN;
        self.retry_at = None;
    }

    pub fn decide(&mut self, state: &CacheState, space: &FreeSpace, now: Instant) -> Decision {
        if !state.culling {
            self.idle();
            return Decision::Idle;
        }
        if let Some(at) = self.retry_at {
            if now < at {
                return Decision::Wait(at);
            }
            self.retry_at = None;
        }
        let want_blocks = state.brun.saturating_add(margin_blocks(state, space));
        let (pending_blocks, pending_files) = self.pending(space, now);
        let blocks = want_blocks
            .saturating_sub(space.blocks_avail)
            .saturating_sub(pending_blocks);
        let files = state
            .frun
            .saturating_sub(space.files_free)
            .saturating_sub(pending_files);
        if blocks == 0 && files == 0 {
            return Decision::Wait(now + RECHECK);
        }
        Decision::Cull(CullTarget {
            bytes: blocks.saturating_mul(space.block_size),
            files,
        })
    }

    /// Blocks and inodes freed in the background by recent passes that
    /// statfs does not show yet. Forgets passes that are shown or older
    /// than the window.
    pub fn pending(&mut self, space: &FreeSpace, now: Instant) -> (u64, u64) {
        while self
            .recent
            .front()
            .is_some_and(|p| now.saturating_duration_since(p.at) >= CONFIRM_WINDOW)
        {
            self.recent.pop_front();
        }
        let Some(first) = self.recent.front() else {
            return (0, 0);
        };
        let gained_blocks = space.blocks_avail.saturating_sub(first.after_blocks);
        let gained_files = space.files_free.saturating_sub(first.after_files);
        let freed_blocks: u64 = self.recent.iter().map(|p| p.blocks).sum();
        let freed_files: u64 = self.recent.iter().map(|p| p.files).sum();
        let pending = (
            freed_blocks.saturating_sub(gained_blocks),
            freed_files.saturating_sub(gained_files),
        );
        if pending == (0, 0) {
            // Everything is visible; later gains are not ours to count.
            self.recent.clear();
        }
        pending
    }

    /// Record a finished pass. `after` is statfs right after it. Returns
    /// the backoff to wait when the pass freed nothing.
    pub fn record(
        &mut self,
        after: &FreeSpace,
        stats: &CullStats,
        now: Instant,
    ) -> Option<Duration> {
        if !stats.made_progress() {
            return Some(self.defer(now));
        }
        self.backoff = BACKOFF_MIN;
        self.retry_at = None;
        if stats.bytes_async > 0 || stats.files_async > 0 {
            self.recent.push_back(Pass {
                at: now,
                after_blocks: after.blocks_avail,
                after_files: after.files_free,
                blocks: stats.bytes_async.div_ceil(after.block_size),
                files: stats.files_async,
            });
        }
        None
    }

    /// Nothing could be done now (no progress, or statfs failed): wait,
    /// doubling the wait each time in a row.
    pub fn defer(&mut self, now: Instant) -> Duration {
        let wait = self.backoff;
        self.backoff = (wait * 2).min(BACKOFF_MAX);
        self.retry_at = Some(now + wait);
        wait
    }
}

fn margin_blocks(state: &CacheState, space: &FreeSpace) -> u64 {
    let band = state.brun.saturating_sub(state.bcull);
    (MARGIN_BYTES / space.block_size).min(band / 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BS: u64 = 4096;

    fn state(culling: bool) -> CacheState {
        // A 100 GiB filesystem: 26214400 blocks of 4 KiB; run/cull/stop at
        // 10/7/3 %.
        CacheState {
            culling,
            frun: 1_000_000,
            fcull: 700_000,
            fstop: 300_000,
            brun: 2_621_440,
            bcull: 1_835_008,
            bstop: 786_432,
        }
    }

    fn space(blocks_avail: u64) -> FreeSpace {
        FreeSpace {
            block_size: BS,
            blocks_total: 26_214_400,
            blocks_avail,
            files_total: 10_000_000,
            files_free: 5_000_000,
        }
    }

    /// A pass whose frees all happen in the background.
    fn freed_async(blocks: u64) -> CullStats {
        CullStats {
            culled: 1,
            bytes_freed: blocks * BS,
            files_freed: 1,
            bytes_async: blocks * BS,
            ..CullStats::default()
        }
    }

    /// The 3 GiB `bcull`..`brun` band caps the margin at a tenth of it,
    /// below `MARGIN_BYTES`.
    fn margin() -> u64 {
        let m = margin_blocks(&state(true), &space(0));
        assert_eq!(m, (state(true).brun - state(true).bcull) / 10);
        m
    }

    #[test]
    fn idle_when_the_kernel_does_not_want_culling() {
        let mut p = Planner::default();
        assert_eq!(
            p.decide(&state(false), &space(0), Instant::now()),
            Decision::Idle
        );
    }

    #[test]
    fn targets_the_shortfall_to_run_plus_margin() {
        let mut p = Planner::default();
        let s = state(true);
        let avail = s.bcull - 1000;
        let d = p.decide(&s, &space(avail), Instant::now());
        let blocks = s.brun + margin() - avail;
        assert_eq!(
            d,
            Decision::Cull(CullTarget {
                bytes: blocks * BS,
                files: 0
            })
        );
    }

    #[test]
    fn margin_is_capped_by_the_band() {
        let mut s = state(true);
        s.bcull = s.brun - 100;
        assert_eq!(margin_blocks(&s, &space(0)), 10);
    }

    #[test]
    fn waits_when_free_space_is_already_at_run() {
        // The kernel counts in-flight writes as used, so it can keep
        // cull=1 after statfs reaches run.
        let mut p = Planner::default();
        let s = state(true);
        let now = Instant::now();
        assert_eq!(
            p.decide(&s, &space(s.brun + margin()), now),
            Decision::Wait(now + RECHECK)
        );
    }

    #[test]
    fn frees_done_on_the_spot_need_no_waiting() {
        let mut p = Planner::default();
        let s = state(true);
        let t0 = Instant::now();
        let stats = CullStats {
            culled: 3,
            bytes_freed: 1000 * BS,
            files_freed: 3,
            ..CullStats::default()
        };
        assert_eq!(p.record(&space(s.bcull), &stats, t0), None);
        assert_eq!(p.pending(&space(s.bcull), t0), (0, 0));
        // statfs right after the pass is the truth: still short, cull again.
        assert!(matches!(
            p.decide(&s, &space(s.bcull), t0),
            Decision::Cull(_)
        ));
    }

    #[test]
    fn subtracts_background_frees_that_statfs_does_not_show_yet() {
        let mut p = Planner::default();
        let s = state(true);
        let t0 = Instant::now();
        let before = space(s.bcull - 1000);
        let Decision::Cull(target) = p.decide(&s, &before, t0) else {
            panic!("expected a pass");
        };
        let blocks = target.bytes / BS;
        // All of it freed in the background: statfs after the pass is
        // unchanged.
        assert_eq!(p.record(&before, &freed_async(blocks), t0), None);

        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(p.decide(&s, &before, t1), Decision::Wait(t1 + RECHECK));

        // Half of it shows up.
        let half = space(before.blocks_avail + blocks / 2);
        let t2 = t0 + Duration::from_secs(10);
        assert_eq!(p.pending(&half, t2).0, blocks - blocks / 2);
        assert_eq!(p.decide(&s, &half, t2), Decision::Wait(t2 + RECHECK));

        // All of it shows up, then 500 blocks get written: those are culled.
        let all = space(before.blocks_avail + blocks);
        assert_eq!(p.pending(&all, t2), (0, 0));
        let written = space(all.blocks_avail - 500);
        match p.decide(&s, &written, t2) {
            Decision::Cull(t) => assert_eq!(t.bytes / BS, 500),
            other => panic!("expected a 500-block pass, got {other:?}"),
        }
    }

    #[test]
    fn writes_masking_background_frees_wait_at_most_the_window() {
        let mut p = Planner::default();
        let s = state(true);
        let t0 = Instant::now();
        let after = space(s.bcull - 10);
        p.record(&after, &freed_async(1_100_000), t0);
        // Writes eat as much as the background frees give back: statfs is
        // flat, and the planner cannot tell the two apart...
        let t1 = t0 + Duration::from_secs(30);
        assert!(matches!(p.decide(&s, &after, t1), Decision::Wait(_)));
        // ...until the window ends.
        let t2 = t0 + CONFIRM_WINDOW;
        assert!(matches!(p.decide(&s, &after, t2), Decision::Cull(_)));
    }

    #[test]
    fn pending_frees_expire_after_the_window() {
        let mut p = Planner::default();
        let t0 = Instant::now();
        let after = space(1000);
        p.record(&after, &freed_async(400), t0);
        assert_eq!(p.pending(&after, t0 + Duration::from_secs(1)).0, 400);
        assert_eq!(p.pending(&after, t0 + CONFIRM_WINDOW).0, 0);
    }

    #[test]
    fn pending_clears_once_visible() {
        let mut p = Planner::default();
        let t0 = Instant::now();
        p.record(&space(1000), &freed_async(400), t0);
        assert_eq!(p.pending(&space(1400), t0).0, 0);
        // Later gains from someone else are not attributed to old passes.
        assert_eq!(p.pending(&space(1000), t0).0, 0);
    }

    #[test]
    fn backoff_doubles_and_resets() {
        let mut p = Planner::default();
        let s = state(true);
        let t0 = Instant::now();
        let nothing = CullStats::default();
        let mut waits = Vec::new();
        for _ in 0..8 {
            waits.push(p.record(&space(0), &nothing, t0).unwrap().as_secs());
        }
        assert_eq!(waits, vec![1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(
            p.decide(&s, &space(0), t0),
            Decision::Wait(t0 + Duration::from_secs(60))
        );

        // Progress resets it.
        let later = t0 + Duration::from_secs(61);
        assert!(matches!(p.decide(&s, &space(0), later), Decision::Cull(_)));
        p.record(&space(0), &freed_async(10), later);
        assert_eq!(p.record(&space(0), &nothing, later), Some(BACKOFF_MIN));

        // So does the kernel clearing cull=1.
        p.decide(&state(false), &space(0), later);
        assert_eq!(p.defer(later), BACKOFF_MIN);
    }

    /// A filesystem whose freed blocks show up in statfs only after `lag`
    /// (frees left to the background), a cache of equal-sized objects, and
    /// the kernel's cull flag.
    struct World {
        now: Instant,
        state: CacheState,
        avail: u64,
        objects: u64,
        object_blocks: u64,
        freeing: VecDeque<(Instant, u64)>,
        lag: Duration,
        culled_blocks: u64,
    }

    impl World {
        fn new(lag: Duration) -> Self {
            let mut state = state(false);
            state.frun = 0;
            state.fcull = 0;
            state.fstop = 0;
            Self {
                now: Instant::now(),
                state,
                avail: state.bcull - 1,
                objects: 1_000,
                object_blocks: 10_000,
                freeing: VecDeque::new(),
                lag,
                culled_blocks: 0,
            }
        }

        fn space(&self) -> FreeSpace {
            let mut s = space(self.avail);
            s.files_free = u64::MAX / 2;
            s
        }

        /// Advance the clock, apply frees that completed, consume `write`
        /// blocks, and update the kernel flag the way cachefiles does.
        fn step(&mut self, dt: Duration, write: u64) {
            self.now += dt;
            while self.freeing.front().is_some_and(|(t, _)| *t <= self.now) {
                self.avail += self.freeing.pop_front().unwrap().1;
            }
            self.avail = self.avail.saturating_sub(write);
            if self.avail < self.state.bcull {
                self.state.culling = true;
            } else if self.state.culling && self.avail >= self.state.brun {
                self.state.culling = false;
            }
        }

        /// Cull oldest objects until `target` is met, like `run_pass`, with
        /// every free left to the background.
        fn cull(&mut self, target: CullTarget) -> CullStats {
            let mut stats = CullStats::default();
            while stats.bytes_freed < target.bytes && self.objects > 0 {
                self.objects -= 1;
                stats.culled += 1;
                stats.files_freed += 1;
                stats.bytes_freed += self.object_blocks * BS;
                self.culled_blocks += self.object_blocks;
                self.freeing
                    .push_back((self.now + self.lag, self.object_blocks));
            }
            stats.bytes_async = stats.bytes_freed;
            stats
        }

        /// Run planner and world for `until`; returns the passes run.
        fn run(&mut self, planner: &mut Planner, until: Duration, write_per_tick: u64) -> usize {
            let end = self.now + until;
            let mut passes = 0;
            let tick = Duration::from_millis(500);
            while self.now < end {
                self.step(tick, write_per_tick);
                let space = self.space();
                if let Decision::Cull(target) = planner.decide(&self.state, &space, self.now) {
                    let stats = self.cull(target);
                    planner.record(&self.space(), &stats, self.now);
                    passes += 1;
                }
            }
            passes
        }
    }

    fn shortfall(w: &World) -> u64 {
        w.state.brun + margin() - (w.state.bcull - 1)
    }

    #[test]
    fn simulated_slow_background_reclaim_does_not_over_cull() {
        // Freed space takes 50 s to show up: longer than the old fixed
        // settle timeout (30 s), after which a second pass was sized on
        // stale statfs and culled the shortfall again.
        let mut w = World::new(Duration::from_secs(50));
        let mut p = Planner::default();
        let passes = w.run(&mut p, Duration::from_secs(300), 0);
        assert!(!w.state.culling, "episode should have ended");
        assert_eq!(passes, 1);
        assert!(w.culled_blocks <= shortfall(&w) + w.object_blocks);
    }

    #[test]
    fn simulated_writes_during_reclaim_are_still_given_back() {
        // Other data keeps arriving while freed space is in flight. The
        // daemon must keep making room without culling the invisible
        // frees twice.
        let mut w = World::new(Duration::from_secs(20));
        let mut p = Planner::default();
        let write = 2_000; // blocks per 0.5 s tick
        let ticks = 600; // 300 s
        w.run(&mut p, Duration::from_millis(500 * ticks), write);
        let written = write * ticks;
        assert!(w.avail >= w.state.bstop, "fell below bstop: {}", w.avail);
        let slack = 3 * w.object_blocks;
        assert!(
            w.culled_blocks <= shortfall(&w) + written + slack,
            "culled {} for shortfall {} + written {written}",
            w.culled_blocks,
            shortfall(&w)
        );
    }

    #[test]
    fn simulated_instant_reclaim_ends_in_one_pass() {
        let mut w = World::new(Duration::ZERO);
        let mut p = Planner::default();
        let passes = w.run(&mut p, Duration::from_secs(10), 0);
        assert_eq!(passes, 1);
        assert!(!w.state.culling);
    }
}
