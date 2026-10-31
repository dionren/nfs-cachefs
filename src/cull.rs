//! Cache cull driver. Walks `<cache_dir>/cache/<volume>/@<bucket>/`, picks
//! the oldest-by-atime cookie objects via a streaming top-K, and sends
//! `cull <name>` commands to the kernel one at a time until the pass has
//! freed what it was asked to (see "How much to cull").
//!
//! ## Cachefiles layout (kernel ≥ 5.17)
//!
//! ```text
//!   <cache_root>/cache/Ivolume/             ← volume index — never cull
//!   <cache_root>/cache/Ivolume/@xx/         ← hash bucket  — never cull
//!   <cache_root>/cache/Ivolume/@xx/Scookie  ← cookie       — cull target
//! ```
//!
//! The walk is therefore restricted to **exactly depth 3** under the
//! cache subdir. Including the volume index as a candidate is unsafe:
//! the kernel only marks it `EBUSY` while a client mount is actively
//! using the volume; between mounts (boot before mount, after umount,
//! e2e setup) `cull Ivolume` succeeds and erases the entire cache.
//! Hash buckets at depth 2 are also not cullable, so we exclude them
//! up front rather than letting the kernel reject them per-pass.
//!
//! ## CWD requirement
//!
//! Each `cull` command resolves its argument relative to the daemon's
//! current working directory (the kernel reads it from `current->fs`).
//! Before each cull we therefore `chdir(parent)`, skipping it when the
//! previous object lived in the same bucket. This is the reason cull is
//! single-threaded: `chdir(2)` mutates a struct that, depending on how
//! threads were created, can be shared between threads. CLAUDE.md flags
//! single-threaded cull as a design invariant; calling `run_pass`
//! concurrently would race even within this process.
//!
//! ## How much to cull
//!
//! The daemon sizes each pass (`planner.rs`): roughly the bytes and inodes
//! missing to bring free space back above `brun`/`frun`. The pass culls
//! oldest-first and stops as soon as that is met. Freed space is counted in
//! allocated bytes (`st_blocks`), not `st_size`: cache files are sparse,
//! often several times larger than what they occupy.
//!
//! ## Freeing the blocks now
//!
//! An unlinked file gives its blocks back only when its last reference
//! goes, and XFS does that work in background workers: a cache file of
//! hundreds of thousands of extents takes seconds, during which statfs —
//! and the kernel's `cull=1` — still count the space as used. The pass
//! therefore opens each candidate before culling it and, once the kernel
//! has unlinked it (`st_nlink` 0 on our descriptor), truncates it through
//! that descriptor. The inode is out of reach for everyone else by then:
//! cachefiles refuses to cull objects it uses (`EBUSY`) and marks culled
//! ones so it never picks them up again. The blocks are free when the
//! truncate returns, so the next pass is sized on accurate numbers.
//!
//! ## Candidates between passes
//!
//! A walk keeps the K oldest objects. What a pass does not use stays in a
//! [`CullTable`] for the next pass (every candidate's atime is re-checked
//! right before it is culled, so a reused list is safe), and a pass walks
//! again only when the list runs out or is older than [`TABLE_TTL`]. K
//! starts at `cull.batch_size` and grows to fit what a pass must free,
//! using the average object size seen by the previous walk.
//!
//! Objects the kernel reports busy, or whose cull fails, are left out of
//! walks for [`SKIP_TTL`]. Without that, a batch made only of busy objects
//! (files tenants keep open or mapped) came back unchanged from every walk,
//! being the oldest, and the cull never reached anything it could free.
//!
//! ## Memory
//!
//! O(K) regardless of cache size, plus the skip set (one hash per object
//! set aside). Candidates share their bucket path.
//!
//! ## Symlinks
//!
//! `walkdir` is configured with `follow_links(false)` and `entry.metadata()`
//! follows the same policy (uses `symlink_metadata` under the hood). The
//! kernel cachefiles backend never creates symlinks in the cache, so this
//! is defense-in-depth.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::proto::{cmd, CacheState, CullOutcome, Kernel};
use crate::systemd_notify::Watchdog;

/// How long the rest of a walk is reused by later passes before the cache
/// is walked again.
pub const TABLE_TTL: Duration = Duration::from_secs(300);

/// How long an object found busy, or failing to cull, is left out of walks.
pub const SKIP_TTL: Duration = Duration::from_secs(300);

/// A walk keeps at most this many times `cull.batch_size` candidates.
const MAX_BATCH_FACTOR: usize = 64;

/// Per-object cull errors logged per pass; the rest are only counted.
const MAX_ERROR_LOGS_PER_PASS: usize = 5;

/// Configurable knobs for one cull pass.
#[derive(Debug, Clone)]
pub struct CullCtx {
    pub cache_root: PathBuf,
    pub batch_size: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CullStats {
    /// Candidates examined (atime re-checked, possibly culled).
    pub candidates: usize,
    pub culled: usize,
    /// Allocated bytes (`st_blocks`) of everything removed, graveyard
    /// included. Not `st_size`: cache files are sparse.
    pub bytes_freed: u64,
    /// Inodes removed: one per culled object, plus graveyard contents.
    pub files_freed: u64,
    pub skipped_busy: usize,
    pub skipped_changed: usize,
    /// Objects that disappeared before we got to them.
    pub gone: usize,
    /// Part of `bytes_freed` / `files_freed` the filesystem releases in
    /// the background (the held-descriptor truncate was not possible), so
    /// statfs may not show it yet.
    pub bytes_async: u64,
    pub files_async: u64,
    pub errored: usize,
    /// Cull commands that failed with `EIO`: the kernel has most likely
    /// stopped the cache. The pass ends at the first one.
    pub kernel_eio: usize,
    pub graveyard_removed: usize,
    /// Walks of the cache directory.
    pub walks: usize,
}

impl CullStats {
    pub fn made_progress(self) -> bool {
        self.culled > 0 || self.graveyard_removed > 0
    }
}

/// Free space on the cache filesystem in the units of the kernel's limits:
/// cachefiles compares `f_bavail` and `f_ffree` from `statfs(cache_dir)`
/// against the `b*` and `f*` values it reports in the state line. With a
/// project quota on `cache_dir` these reflect the quota (see `quota.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeSpace {
    /// Bytes per block of the block counts (statvfs `f_frsize`).
    pub block_size: u64,
    pub blocks_total: u64,
    pub blocks_avail: u64,
    pub files_total: u64,
    pub files_free: u64,
}

impl FreeSpace {
    // statvfs field types are platform-dependent; already u64 on 64-bit Linux.
    #[allow(clippy::useless_conversion)]
    pub fn of(cache_root: &Path) -> io::Result<Self> {
        let st = nix::sys::statvfs::statvfs(cache_root)?;
        let block_size = match u64::from(st.fragment_size()) {
            0 => u64::from(st.block_size()),
            n => n,
        };
        if block_size == 0 {
            return Err(io::Error::other("statvfs reported a zero block size"));
        }
        Ok(Self {
            block_size,
            blocks_total: u64::from(st.blocks()),
            blocks_avail: u64::from(st.blocks_available()),
            files_total: u64::from(st.files()),
            files_free: u64::from(st.files_free()),
        })
    }
}

/// What a cull pass must free.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CullTarget {
    pub bytes: u64,
    pub files: u64,
}

impl CullTarget {
    /// The bytes and inodes missing to bring free space back up to
    /// `brun`/`frun`, the point where the kernel clears `cull=1`.
    pub fn new(state: &CacheState, space: &FreeSpace) -> Self {
        Self {
            bytes: state
                .brun
                .saturating_sub(space.blocks_avail)
                .saturating_mul(space.block_size),
            files: state.frun.saturating_sub(space.files_free),
        }
    }

    pub fn is_zero(self) -> bool {
        self.bytes == 0 && self.files == 0
    }

    fn met_by(self, stats: &CullStats) -> bool {
        stats.bytes_freed >= self.bytes && stats.files_freed >= self.files
    }
}

/// Objects and allocated bytes in the cache as of `at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub objects: u64,
    pub allocated_bytes: u64,
    pub at: Instant,
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Candidate {
    // atime_secs is the primary sort key. Putting it first makes the
    // derived Ord rank by it, which is what the BinaryHeap relies on:
    // as a max-heap, the root is the YOUNGEST of the K kept so far —
    // the one we should evict if a still-older file shows up.
    atime_secs: i64,
    atime_nsecs: i64,
    /// Allocated bytes, what culling this object gives back. For an index
    /// cookie directory only its own blocks: the kernel moves its subtree
    /// to the graveyard, which is counted when the graveyard is emptied.
    allocated: u64,
    /// The bucket directory, shared by all candidates from it.
    parent: Rc<Path>,
    name: String,
}

impl Candidate {
    fn path(&self) -> PathBuf {
        self.parent.join(&self.name)
    }

    fn key(&self) -> u64 {
        object_key(&self.parent, &self.name)
    }

    fn atime_changed(&self) -> std::io::Result<bool> {
        let meta = std::fs::symlink_metadata(self.path())?;
        Ok(meta.atime() != self.atime_secs || meta.atime_nsec() != self.atime_nsecs)
    }

    fn is_older_than(&self, other: &Self) -> bool {
        (self.atime_secs, self.atime_nsecs) < (other.atime_secs, other.atime_nsecs)
    }
}

fn object_key(parent: &Path, name: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    parent.hash(&mut hasher);
    name.hash(&mut hasher);
    hasher.finish()
}

/// State kept between cull passes: the unused rest of the last walk, the
/// objects set aside as busy or failing, and what the walk learned about
/// the cache.
#[derive(Debug, Default)]
pub struct CullTable {
    queue: VecDeque<Candidate>,
    walked_at: Option<Instant>,
    /// Object key → until when it stays out of walks.
    skip: HashMap<u64, Instant>,
    avg_allocated: Option<u64>,
    usage: Option<Usage>,
}

impl CullTable {
    /// Cache size as of the last complete walk.
    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }

    /// Objects currently left out of walks.
    pub fn skipped(&self) -> usize {
        self.skip.len()
    }

    fn expire(&mut self, now: Instant) {
        let stale = self
            .walked_at
            .map_or(true, |at| now.saturating_duration_since(at) >= TABLE_TTL);
        if stale {
            self.queue.clear();
        }
        self.skip.retain(|_, until| *until > now);
    }

    /// Candidates to keep from the next walk: `base`, or enough to cover
    /// what is left to free if the objects look like the last walk's.
    fn batch_size(&self, base: usize, remaining_bytes: u64, remaining_files: u64) -> usize {
        let by_bytes = match self.avg_allocated {
            Some(avg) if avg > 0 => remaining_bytes.div_ceil(avg),
            _ => 0,
        };
        let want = by_bytes.max(remaining_files).saturating_mul(5) / 4;
        let max = base.saturating_mul(MAX_BATCH_FACTOR);
        usize::try_from(want).unwrap_or(usize::MAX).clamp(base, max)
    }

    fn note_walk(&mut self, walk: &Walk, now: Instant) {
        if let Some(avg) = walk.allocated.checked_div(walk.objects) {
            self.avg_allocated = Some(avg);
        }
        if walk.complete {
            self.usage = Some(Usage {
                objects: walk.objects,
                allocated_bytes: walk.allocated,
                at: now,
            });
        }
        self.walked_at = Some(now);
    }

    fn set_aside(&mut self, cand: &Candidate, now: Instant) {
        self.skip.insert(cand.key(), now + SKIP_TTL);
    }
}

/// Run one cull pass: cull oldest-by-atime objects until `target` has been
/// freed, taking candidates from `table` and walking the cache when it runs
/// out. The pass also ends on stop, on the first `EIO`, or when a batch
/// frees nothing and sets nothing new aside (everything left is recently
/// used).
///
/// Per-object errors (EBUSY, chdir failure, etc.) are logged and counted
/// but never propagated: the daemon sizes another pass if more is needed,
/// and a single bad object should not bring the daemon down.
pub fn run_pass(
    dev: &impl Kernel,
    ctx: &CullCtx,
    table: &mut CullTable,
    target: CullTarget,
    stop: &AtomicBool,
    wd: &Watchdog,
) -> CullStats {
    let saved_cwd = std::env::current_dir().ok();
    let mut cwd: Option<Rc<Path>> = None;
    let stats = run_pass_with(ctx, table, target, stop, wd, |cand| {
        let same_dir = cwd
            .as_ref()
            .is_some_and(|dir| Rc::ptr_eq(dir, &cand.parent) || **dir == *cand.parent);
        if !same_dir {
            std::env::set_current_dir(&cand.parent).map_err(|e| {
                Error::Io(io::Error::new(
                    e.kind(),
                    format!("chdir {}: {e}", cand.parent.display()),
                ))
            })?;
            cwd = Some(Rc::clone(&cand.parent));
        }
        cmd::cull(dev, &cand.name)
    });
    if let Some(dir) = saved_cwd {
        if let Err(e) = std::env::set_current_dir(&dir) {
            warn!(cwd = %dir.display(), error = %e, "failed to restore cwd");
        }
    }
    stats
}

/// [`run_pass`] with the kernel `cull` command passed in, so the pass logic
/// can be tested without `/dev/cachefiles`.
fn run_pass_with(
    ctx: &CullCtx,
    table: &mut CullTable,
    target: CullTarget,
    stop: &AtomicBool,
    wd: &Watchdog,
    mut cull_one: impl FnMut(&Candidate) -> Result<CullOutcome>,
) -> CullStats {
    let started = Instant::now();
    let mut stats = clean_graveyard(&ctx.cache_root, Some(stop), wd);
    let cache_subdir = ctx.cache_root.join("cache");
    if !cache_subdir.is_dir() {
        warn!(
            path = %cache_subdir.display(),
            "cache subdir does not exist; kernel layout changed or bind never created it"
        );
        return stats;
    }
    table.expire(started);
    let mut errors = ErrorLog::default();

    'pass: while !target.met_by(&stats) && !stop.load(Ordering::Relaxed) {
        if table.queue.is_empty() {
            let k = table.batch_size(
                ctx.batch_size,
                target.bytes.saturating_sub(stats.bytes_freed),
                target.files.saturating_sub(stats.files_freed),
            );
            let walk = collect_oldest(&cache_subdir, k, &table.skip, Some(stop), wd);
            stats.walks += 1;
            table.note_walk(&walk, Instant::now());
            debug!(
                found = walk.candidates.len(),
                objects = walk.objects,
                batch = k,
                set_aside = table.skip.len(),
                "cull candidates"
            );
            if walk.candidates.is_empty() {
                break;
            }
            table.queue = walk.candidates.into();
        }

        let culled_before = stats.culled;
        let set_aside_before = table.skip.len();
        while let Some(cand) = table.queue.pop_front() {
            if target.met_by(&stats) || stop.load(Ordering::Relaxed) {
                table.queue.push_front(cand);
                break 'pass;
            }
            wd.kick();
            stats.candidates += 1;
            match cand.atime_changed() {
                Ok(false) => {}
                Ok(true) => {
                    stats.skipped_changed += 1;
                    continue;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    stats.gone += 1;
                    continue;
                }
                Err(e) => {
                    stats.errored += 1;
                    table.set_aside(&cand, Instant::now());
                    errors.log(&cand.name, "metadata recheck failed; skipping", &e);
                    continue;
                }
            }
            let held = hold_for_reclaim(&cand.path());
            match cull_one(&cand) {
                Ok(CullOutcome::Culled) => {
                    let (bytes, now_free) = reclaim(held, cand.allocated);
                    stats.culled += 1;
                    stats.bytes_freed += bytes;
                    stats.files_freed += 1;
                    if !now_free {
                        stats.bytes_async += bytes;
                        stats.files_async += 1;
                    }
                }
                Ok(CullOutcome::Busy) => {
                    stats.skipped_busy += 1;
                    table.set_aside(&cand, Instant::now());
                }
                Ok(CullOutcome::Gone) => stats.gone += 1,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    // Usually the kernel has stopped the cache, and every
                    // further command would fail the same way. The daemon
                    // checks the cache state.
                    stats.kernel_eio += 1;
                    table.set_aside(&cand, Instant::now());
                    warn!(name = cand.name, error = %e, "cull failed with EIO; ending pass");
                    break 'pass;
                }
                Err(e) => {
                    stats.errored += 1;
                    table.set_aside(&cand, Instant::now());
                    errors.log(&cand.name, "cull failed", &e);
                }
            }
        }

        // The batch is used up. If it freed nothing and set nothing new
        // aside, walking again would return the same objects.
        if stats.culled == culled_before && table.skip.len() == set_aside_before {
            break;
        }
    }
    errors.finish();

    let elapsed_ms = started.elapsed().as_millis() as u64;
    macro_rules! pass_done {
        ($level:ident) => {
            $level!(
                elapsed_ms,
                target_bytes = target.bytes,
                target_files = target.files,
                culled = stats.culled,
                bytes_freed = stats.bytes_freed,
                skipped_busy = stats.skipped_busy,
                skipped_changed = stats.skipped_changed,
                gone = stats.gone,
                errored = stats.errored,
                graveyard_removed = stats.graveyard_removed,
                walks = stats.walks,
                "cull pass done"
            )
        };
    }
    if stats.made_progress() {
        pass_done!(info);
    } else {
        pass_done!(debug);
    }
    stats
}

/// Open `path` so its inode outlives the unlink. `None` for directories
/// (index cookies) or when it cannot be opened; the object is then freed
/// by the filesystem in the background as usual.
fn hold_for_reclaim(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()
}

/// Free the blocks of a file that was just culled (or deleted from the
/// graveyard) through the descriptor that held it. Returns the allocated
/// bytes released and whether they are free already (`false`: the
/// filesystem frees them in the background). Truncates only an inode that
/// no directory links to any more, so nothing else can reach it.
fn reclaim(held: Option<std::fs::File>, walked: u64) -> (u64, bool) {
    let Some(file) = held else {
        return (walked, false);
    };
    let Ok(meta) = file.metadata() else {
        return (walked, false);
    };
    let bytes = allocated_bytes(&meta);
    if !meta.is_file() || meta.nlink() != 0 {
        return (bytes, false);
    }
    match file.set_len(0) {
        Ok(()) => (bytes, true),
        Err(e) => {
            debug!(error = %e, "truncate of culled file failed; space is freed in the background");
            (bytes, false)
        }
    }
}

/// Logs the first few per-object errors of a pass and counts the rest.
#[derive(Default)]
struct ErrorLog {
    seen: usize,
}

impl ErrorLog {
    fn log(&mut self, name: &str, what: &str, err: &dyn fmt::Display) {
        self.seen += 1;
        if self.seen <= MAX_ERROR_LOGS_PER_PASS {
            warn!(name, error = %err, "{what}");
        }
    }

    fn finish(self) {
        if self.seen > MAX_ERROR_LOGS_PER_PASS {
            warn!(
                not_logged = self.seen - MAX_ERROR_LOGS_PER_PASS,
                "further per-object cull errors in this pass"
            );
        }
    }
}

/// One walk of the cache objects.
#[derive(Debug)]
pub(crate) struct Walk {
    /// The `k` oldest objects not set aside, oldest first.
    candidates: Vec<Candidate>,
    /// Every object seen, set aside or not.
    objects: u64,
    allocated: u64,
    /// False when the stop flag cut the walk short.
    complete: bool,
}

/// Walk the cache subtree and return the `k` oldest-by-atime objects in
/// ascending-atime order, leaving out those in `skip`. Uses a max-heap of
/// size k → O(N log k) time, O(k) memory regardless of cache size.
fn collect_oldest(
    root: &Path,
    k: usize,
    skip: &HashMap<u64, Instant>,
    stop: Option<&AtomicBool>,
    wd: &Watchdog,
) -> Walk {
    let mut walk = Walk {
        candidates: Vec::new(),
        objects: 0,
        allocated: 0,
        complete: true,
    };
    let mut heap: BinaryHeap<Candidate> = BinaryHeap::with_capacity(k.min(1 << 16));
    let mut last_parent: Option<Rc<Path>> = None;

    // Depth 3 = cookie objects (see module docs for the layout). Both
    // bounds are required: min_depth(3) skips the volume index (depth 1)
    // and hash buckets (depth 2); max_depth(3) prevents descending into
    // an index-cookie directory and culling its children individually
    // — culling the parent removes the whole subtree.
    let entries = walkdir::WalkDir::new(root)
        .min_depth(3)
        .max_depth(3)
        .follow_links(false)
        .same_file_system(true)
        .into_iter();

    for entry in entries {
        if stop_requested(stop) {
            walk.complete = false;
            break;
        }
        wd.kick();
        let Ok(entry) = entry else { continue };
        let file_type = entry.file_type();
        if !(file_type.is_file() || file_type.is_dir()) {
            continue;
        }
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
        if !is_cache_object_name(name) {
            continue;
        }
        let Some(parent) = entry.path().parent() else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let allocated = allocated_bytes(&meta);
        walk.objects += 1;
        walk.allocated += allocated;

        let parent = match &last_parent {
            Some(last) if **last == *parent => Rc::clone(last),
            _ => {
                let shared: Rc<Path> = Rc::from(parent);
                last_parent = Some(Rc::clone(&shared));
                shared
            }
        };
        if !skip.is_empty() && skip.contains_key(&object_key(&parent, name)) {
            continue;
        }
        let cand = Candidate {
            atime_secs: meta.atime(),
            atime_nsecs: meta.atime_nsec(),
            allocated,
            parent,
            name: name.to_string(),
        };
        if heap.len() < k {
            heap.push(cand);
        } else if let Some(top) = heap.peek() {
            // Heap root = youngest of the K so far. If this candidate is
            // older, evict the youngest and keep this one.
            if cand.is_older_than(top) {
                heap.pop();
                heap.push(cand);
            }
        }
    }

    // Sorts ascending by Ord (atime_secs first) → oldest first.
    walk.candidates = heap.into_sorted_vec();
    walk
}

/// Objects and allocated bytes in the cache, by walking it. `None` when the
/// stop flag cut the walk short.
pub fn cache_usage(cache_root: &Path, stop: &AtomicBool, wd: &Watchdog) -> Option<Usage> {
    let walk = collect_oldest(
        &cache_root.join("cache"),
        0,
        &HashMap::new(),
        Some(stop),
        wd,
    );
    walk.complete.then(|| Usage {
        objects: walk.objects,
        allocated_bytes: walk.allocated,
        at: Instant::now(),
    })
}

fn stop_requested(stop: Option<&AtomicBool>) -> bool {
    stop.is_some_and(|stop| stop.load(Ordering::Relaxed))
}

fn is_cache_object_name(name: &str) -> bool {
    matches!(
        name.as_bytes().first(),
        Some(b'I' | b'J' | b'D' | b'E' | b'S' | b'T')
    )
}

/// Delete what the kernel moved into `<cache_root>/graveyard`. Each entry is
/// removed contents first, one file at a time, so a large buried tree does
/// not block the stop flag or the watchdog; what is left is finished on the
/// next call.
pub fn clean_graveyard(cache_root: &Path, stop: Option<&AtomicBool>, wd: &Watchdog) -> CullStats {
    let mut stats = CullStats::default();
    let graveyard = cache_root.join("graveyard");
    let Ok(entries) = std::fs::read_dir(&graveyard) else {
        return stats;
    };

    for entry in entries {
        if stop_requested(stop) {
            break;
        }
        let Ok(entry) = entry else {
            stats.errored += 1;
            continue;
        };
        let path = entry.path();
        let mut complete = true;
        let mut first_error: Option<io::Error> = None;
        let items = walkdir::WalkDir::new(&path)
            .contents_first(true)
            .follow_links(false)
            .same_file_system(true);
        for item in items {
            if stop_requested(stop) {
                complete = false;
                break;
            }
            wd.kick();
            let item = match item {
                Ok(item) => item,
                Err(e) => {
                    complete = false;
                    first_error.get_or_insert_with(|| io::Error::other(e.to_string()));
                    continue;
                }
            };
            // Measured before removal so a cull pass can count it toward
            // its target.
            let bytes = item.metadata().map(|m| allocated_bytes(&m)).unwrap_or(0);
            let is_dir = item.file_type().is_dir();
            let held = if item.file_type().is_file() {
                hold_for_reclaim(item.path())
            } else {
                None
            };
            let removed = if is_dir {
                std::fs::remove_dir(item.path())
            } else {
                std::fs::remove_file(item.path())
            };
            match removed {
                Ok(()) => {
                    let (bytes, now_free) = if held.is_some() {
                        reclaim(held, bytes)
                    } else {
                        (bytes, false)
                    };
                    stats.bytes_freed += bytes;
                    stats.files_freed += 1;
                    if !now_free {
                        stats.bytes_async += bytes;
                        stats.files_async += 1;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    complete = false;
                    first_error.get_or_insert(e);
                }
            }
        }
        if let Some(e) = first_error {
            stats.errored += 1;
            warn!(path = %path.display(), error = %e, "failed to remove graveyard entry");
        }
        if complete {
            stats.graveyard_removed += 1;
        }
    }
    stats
}

/// Space an object occupies on disk. `st_blocks` counts 512-byte units
/// whatever the filesystem block size is.
fn allocated_bytes(meta: &std::fs::Metadata) -> u64 {
    meta.blocks().saturating_mul(512)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::time::SystemTime;

    fn touch(path: &Path, atime_offset_secs: i64) {
        // A full block, so the object has allocated space for a pass to free.
        fs::write(path, [b'x'; 4096]).unwrap();
        let f = fs::File::options().write(true).open(path).unwrap();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let at = if atime_offset_secs >= 0 {
            base + Duration::from_secs(atime_offset_secs as u64)
        } else {
            base - Duration::from_secs((-atime_offset_secs) as u64)
        };
        f.set_times(fs::FileTimes::new().set_accessed(at).set_modified(at))
            .unwrap();
    }

    /// Build a fixture matching the cachefiles layout
    /// `<root>/Ivolume/@<bucket>/<cookie>` and return the bucket path so
    /// callers can drop cookie files in the right place.
    fn cookie_bucket(root: &Path) -> PathBuf {
        let bucket = root.join("Ivolume").join("@00");
        fs::create_dir_all(&bucket).unwrap();
        bucket
    }

    fn oldest(root: &Path, k: usize) -> Vec<Candidate> {
        collect_oldest(root, k, &HashMap::new(), None, &Watchdog::disabled()).candidates
    }

    #[test]
    fn top_k_returns_k_oldest_in_ascending_order() {
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        for (n, off) in [("Da", 100), ("Db", 50), ("Dc", 200), ("Dd", 25), ("De", 75)] {
            touch(&bucket.join(n), off);
        }
        let names: Vec<_> = oldest(&dir, 3).into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["Dd", "Db", "De"]);
    }

    #[test]
    fn top_k_handles_k_greater_than_n() {
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        touch(&bucket.join("Da"), 10);
        touch(&bucket.join("Db"), 20);
        let found = oldest(&dir, 100);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "Da");
        assert_eq!(found[1].name, "Db");
    }

    #[test]
    fn top_k_zero_keeps_nothing_but_counts_usage() {
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        touch(&bucket.join("Da"), 10);
        let walk = collect_oldest(&dir, 0, &HashMap::new(), None, &Watchdog::disabled());
        assert!(walk.candidates.is_empty());
        assert_eq!(walk.objects, 1);
        assert!(walk.allocated > 0);
        assert!(walk.complete);
    }

    #[test]
    fn top_k_leaves_out_objects_set_aside() {
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        touch(&bucket.join("Da"), 10);
        touch(&bucket.join("Db"), 20);
        let mut skip = HashMap::new();
        skip.insert(
            object_key(&bucket, "Da"),
            Instant::now() + Duration::from_secs(60),
        );
        let walk = collect_oldest(&dir, 10, &skip, None, &Watchdog::disabled());
        let names: Vec<_> = walk.candidates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Db"]);
        assert_eq!(
            walk.objects, 2,
            "set-aside objects still count as cache usage"
        );
    }

    #[test]
    fn top_k_skips_volume_index_and_hash_buckets() {
        // Regression: previously the depth-1 `Ivolume` directory matched
        // `is_cache_object_name` (prefix `I`) and ended up as a cull
        // candidate. With the volume unbound (e.g., between client
        // mounts) the kernel let the cull through and emptied the cache.
        let dir = tempdir();
        let volume = dir.join("Ivolume,uniq");
        let bucket = volume.join("@1d");
        fs::create_dir_all(&bucket).unwrap();
        touch(&bucket.join("Scookie"), 100);

        let names: Vec<_> = oldest(&dir, 10).into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["Scookie"]);
    }

    #[test]
    fn top_k_does_not_descend_into_index_cookie_subtree() {
        // An index-style cookie can be a directory at depth 3 with its
        // own children at depth 4+. The daemon must enqueue only the
        // depth-3 cookie itself; culling it removes the subtree.
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        let index_cookie = bucket.join("Iindex");
        fs::create_dir_all(&index_cookie).unwrap();
        touch(&index_cookie.join("Schild"), 1);
        touch(&bucket.join("Scookie"), 2);

        let mut names: Vec<_> = oldest(&dir, 10).into_iter().map(|c| c.name).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["Iindex", "Scookie"]);
    }

    #[test]
    fn candidates_from_one_bucket_share_its_path() {
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        touch(&bucket.join("Da"), 1);
        touch(&bucket.join("Db"), 2);
        let found = oldest(&dir, 10);
        assert!(Rc::ptr_eq(&found[0].parent, &found[1].parent));
    }

    #[test]
    fn graveyard_cleanup_removes_entries() {
        let dir = tempdir();
        let graveyard = dir.join("graveyard");
        fs::create_dir_all(graveyard.join("dead-dir").join("sub")).unwrap();
        fs::write(graveyard.join("dead-dir").join("child"), b"x").unwrap();
        fs::write(graveyard.join("dead-dir").join("sub").join("deeper"), b"x").unwrap();
        fs::write(graveyard.join("dead-file"), b"x").unwrap();

        let stats = clean_graveyard(&dir, None, &Watchdog::disabled());
        assert_eq!(stats.graveyard_removed, 2);
        // dead-dir, sub, child, deeper, dead-file
        assert_eq!(stats.files_freed, 5);
        // The three files are truncated through a held descriptor; the
        // two directories are left to the filesystem.
        assert_eq!(stats.files_async, 2);
        assert_eq!(stats.errored, 0);
        assert!(fs::read_dir(&graveyard).unwrap().next().is_none());
    }

    #[test]
    fn graveyard_cleanup_stops_on_signal() {
        let dir = tempdir();
        let graveyard = dir.join("graveyard");
        fs::create_dir_all(graveyard.join("dead-dir")).unwrap();
        fs::write(graveyard.join("dead-dir").join("child"), b"x").unwrap();
        let stop = AtomicBool::new(true);
        let stats = clean_graveyard(&dir, Some(&stop), &Watchdog::disabled());
        assert_eq!(stats.graveyard_removed, 0);
        assert!(graveyard.join("dead-dir").join("child").exists());
    }

    #[test]
    fn candidate_recheck_detects_atime_change() {
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        let path = bucket.join("Scookie");
        touch(&path, 10);

        let found = oldest(&dir, 1);
        assert_eq!(found.len(), 1);
        assert!(!found[0].atime_changed().unwrap());

        touch(&path, 20);
        assert!(found[0].atime_changed().unwrap());
    }

    fn pass_ctx(root: &Path, batch_size: usize) -> CullCtx {
        CullCtx {
            cache_root: root.to_path_buf(),
            batch_size,
        }
    }

    /// `<root>/cache/Ivolume/@00/` holding one cookie per `(name, atime)`,
    /// the layout `run_pass` walks. Returns the bucket.
    fn cache_with(root: &Path, cookies: &[(&str, i64)]) -> PathBuf {
        let bucket = cookie_bucket(&root.join("cache"));
        for (name, off) in cookies {
            touch(&bucket.join(name), *off);
        }
        bucket
    }

    /// Stands in for the kernel: culls by unlinking, reports `busy` names
    /// as held, and records every name it was asked about.
    fn fake_cull<'a>(
        busy: &'a [&'a str],
        asked: &'a RefCell<Vec<String>>,
    ) -> impl FnMut(&Candidate) -> Result<CullOutcome> + 'a {
        move |cand: &Candidate| {
            asked.borrow_mut().push(cand.name.clone());
            if busy.contains(&cand.name.as_str()) {
                return Ok(CullOutcome::Busy);
            }
            fs::remove_file(cand.path())?;
            Ok(CullOutcome::Culled)
        }
    }

    fn pass(
        ctx: &CullCtx,
        table: &mut CullTable,
        target: CullTarget,
        busy: &[&str],
    ) -> (CullStats, Vec<String>) {
        let asked = RefCell::new(Vec::new());
        let stop = AtomicBool::new(false);
        let stats = run_pass_with(
            ctx,
            table,
            target,
            &stop,
            &Watchdog::disabled(),
            fake_cull(busy, &asked),
        );
        (stats, asked.into_inner())
    }

    fn files(n: u64) -> CullTarget {
        CullTarget { bytes: 0, files: n }
    }

    fn allocated_of(bucket: &Path, names: &[&str]) -> u64 {
        names
            .iter()
            .map(|n| allocated_bytes(&fs::metadata(bucket.join(n)).unwrap()))
            .sum()
    }

    fn remaining(bucket: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(bucket)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn pass_stops_once_target_is_freed() {
        // Regression: a pass used to cull its whole batch (up to 1024
        // objects) however little the kernel needed, which emptied caches
        // made of a few thousand large files.
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10), ("Db", 20), ("Dc", 30), ("Dd", 40)]);
        let need = allocated_of(&bucket, &["Da", "Db"]);
        assert!(need > 0);

        let target = CullTarget {
            bytes: need,
            files: 0,
        };
        let (stats, _) = pass(
            &pass_ctx(&dir, 1024),
            &mut CullTable::default(),
            target,
            &[],
        );

        assert_eq!(stats.culled, 2);
        assert_eq!(stats.bytes_freed, need);
        assert_eq!(remaining(&bucket), vec!["Dc", "Dd"]);
    }

    #[test]
    fn pass_walks_again_when_one_batch_is_not_enough() {
        let dir = tempdir();
        let bucket = cache_with(
            &dir,
            &[("Da", 10), ("Db", 20), ("Dc", 30), ("Dd", 40), ("De", 50)],
        );
        // Nothing is known about object sizes before the first walk, so it
        // keeps batch_size (2) candidates; the second walk finds the third.
        let target = CullTarget {
            bytes: allocated_of(&bucket, &["Da", "Db", "Dc"]),
            files: 0,
        };
        let (stats, _) = pass(&pass_ctx(&dir, 2), &mut CullTable::default(), target, &[]);

        assert_eq!(stats.culled, 3);
        assert_eq!(stats.walks, 2);
        assert_eq!(remaining(&bucket), vec!["Dd", "De"]);
    }

    #[test]
    fn an_inode_target_sizes_the_first_walk() {
        let dir = tempdir();
        let bucket = cache_with(
            &dir,
            &[("Da", 10), ("Db", 20), ("Dc", 30), ("Dd", 40), ("De", 50)],
        );
        let (stats, _) = pass(&pass_ctx(&dir, 2), &mut CullTable::default(), files(3), &[]);
        assert_eq!((stats.culled, stats.walks), (3, 1));
        assert_eq!(remaining(&bucket), vec!["Dd", "De"]);
    }

    #[test]
    fn pass_gets_past_a_busy_oldest_batch() {
        // Regression: when every object of the oldest batch was busy, the
        // pass ended without progress, and every later walk returned the
        // same busy objects (still the oldest), so nothing was ever culled.
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10), ("Db", 20), ("Dc", 30), ("Dd", 40)]);

        let (stats, asked) = pass(
            &pass_ctx(&dir, 2),
            &mut CullTable::default(),
            files(1),
            &["Da", "Db"],
        );

        assert_eq!(stats.culled, 1);
        assert_eq!(stats.skipped_busy, 2);
        assert_eq!(asked, vec!["Da", "Db", "Dc"]);
        assert_eq!(remaining(&bucket), vec!["Da", "Db", "Dd"]);
    }

    #[test]
    fn busy_objects_stay_set_aside_on_the_next_pass() {
        let dir = tempdir();
        cache_with(&dir, &[("Da", 10), ("Db", 20), ("Dc", 30), ("Dd", 40)]);
        let ctx = pass_ctx(&dir, 1024);
        let mut table = CullTable::default();

        let (first, _) = pass(&ctx, &mut table, files(1), &["Da"]);
        assert_eq!((first.culled, first.skipped_busy), (1, 1));
        assert_eq!(table.skipped(), 1);

        let (second, asked) = pass(&ctx, &mut table, files(1), &["Da"]);
        assert_eq!(second.culled, 1);
        assert_eq!(asked, vec!["Dc"], "Da is not asked about again");
    }

    #[test]
    fn set_aside_objects_come_back_after_skip_ttl() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10)]);
        let mut table = CullTable::default();
        let walk = collect_oldest(
            &dir.join("cache"),
            10,
            &table.skip,
            None,
            &Watchdog::disabled(),
        );
        let now = Instant::now();
        table.set_aside(&walk.candidates[0], now);
        assert_eq!(table.skipped(), 1);
        table.expire(now + SKIP_TTL);
        assert_eq!(table.skipped(), 0);
        assert_eq!(remaining(&bucket), vec!["Da"]);
    }

    #[test]
    fn pass_reuses_the_rest_of_the_previous_walk() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10), ("Db", 20), ("Dc", 30)]);
        let ctx = pass_ctx(&dir, 1024);
        let mut table = CullTable::default();

        let (first, _) = pass(&ctx, &mut table, files(1), &[]);
        assert_eq!((first.culled, first.walks), (1, 1));

        let (second, asked) = pass(&ctx, &mut table, files(1), &[]);
        assert_eq!(second.culled, 1);
        assert_eq!(second.walks, 0, "candidates left from the first walk");
        assert_eq!(asked, vec!["Db"]);
        assert_eq!(remaining(&bucket), vec!["Dc"]);

        // Too old to trust: the next pass walks again.
        table.expire(Instant::now() + TABLE_TTL);
        let (third, _) = pass(&ctx, &mut table, files(1), &[]);
        assert_eq!((third.culled, third.walks), (1, 1));
    }

    #[test]
    fn walk_grows_to_cover_the_target() {
        let dir = tempdir();
        let cookies: Vec<(String, i64)> = (0..10).map(|i| (format!("D{i:02}"), i)).collect();
        let refs: Vec<(&str, i64)> = cookies.iter().map(|(n, a)| (n.as_str(), *a)).collect();
        let bucket = cache_with(&dir, &refs);
        let oldest_eight: Vec<&str> = refs.iter().take(8).map(|(n, _)| *n).collect();

        let target = CullTarget {
            bytes: allocated_of(&bucket, &oldest_eight),
            files: 0,
        };
        let (stats, _) = pass(&pass_ctx(&dir, 2), &mut CullTable::default(), target, &[]);

        assert_eq!(stats.culled, 8);
        // First walk: batch_size 2. It shows the object size, so the
        // second walk keeps enough for the remaining six.
        assert_eq!(stats.walks, 2);
    }

    #[test]
    fn batch_size_is_bounded() {
        let mut table = CullTable::default();
        assert_eq!(table.batch_size(100, 1 << 40, 0), 100, "no size known yet");
        table.avg_allocated = Some(1 << 20);
        assert_eq!(table.batch_size(100, 10 << 20, 0), 100);
        assert_eq!(table.batch_size(100, 1000 << 20, 0), 1250);
        assert_eq!(table.batch_size(100, 1 << 50, 0), 6400);
        assert_eq!(table.batch_size(100, 0, 500), 625);
    }

    #[test]
    fn culled_files_are_freed_through_the_held_descriptor() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10), ("Db", 20)]);
        let need = allocated_of(&bucket, &["Da"]);
        let target = CullTarget {
            bytes: need,
            files: 0,
        };
        let (stats, _) = pass(
            &pass_ctx(&dir, 1024),
            &mut CullTable::default(),
            target,
            &[],
        );
        assert_eq!(stats.culled, 1);
        assert_eq!(stats.bytes_freed, need);
        assert_eq!((stats.bytes_async, stats.files_async), (0, 0));
    }

    #[test]
    fn a_file_still_linked_after_cull_is_not_truncated() {
        // Should the kernel report success yet the name still point at our
        // inode (a race with object replacement), the data must stay.
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10)]);
        let size = fs::metadata(bucket.join("Da")).unwrap().len();
        let stop = AtomicBool::new(false);
        let stats = run_pass_with(
            &pass_ctx(&dir, 1024),
            &mut CullTable::default(),
            files(1),
            &stop,
            &Watchdog::disabled(),
            |_| Ok(CullOutcome::Culled),
        );
        assert_eq!(stats.culled, 1);
        assert_eq!(stats.bytes_async, stats.bytes_freed);
        assert_eq!(fs::metadata(bucket.join("Da")).unwrap().len(), size);
    }

    #[test]
    fn eio_ends_the_pass() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10), ("Db", 20)]);
        let stop = AtomicBool::new(false);
        let mut asked = 0;
        let stats = run_pass_with(
            &pass_ctx(&dir, 1024),
            &mut CullTable::default(),
            files(2),
            &stop,
            &Watchdog::disabled(),
            |_| {
                asked += 1;
                Err(Error::Kernel {
                    cmd: "cull x".into(),
                    source: io::Error::from_raw_os_error(libc::EIO),
                })
            },
        );
        assert_eq!(asked, 1);
        assert_eq!(stats.kernel_eio, 1);
        assert!(!stats.made_progress());
        assert_eq!(remaining(&bucket), vec!["Da", "Db"]);
    }

    #[test]
    fn vanished_objects_are_not_errors() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10), ("Db", 20)]);
        let stop = AtomicBool::new(false);
        let stats = run_pass_with(
            &pass_ctx(&dir, 1024),
            &mut CullTable::default(),
            files(1),
            &stop,
            &Watchdog::disabled(),
            |cand| {
                if cand.name == "Da" {
                    return Ok(CullOutcome::Gone);
                }
                fs::remove_file(cand.path())?;
                Ok(CullOutcome::Culled)
            },
        );
        assert_eq!((stats.gone, stats.errored, stats.culled), (1, 0, 1));
        assert_eq!(remaining(&bucket), vec!["Da"]);
    }

    #[test]
    fn pass_skips_busy_objects() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10), ("Db", 20), ("Dc", 30), ("Dd", 40)]);

        let (stats, _) = pass(
            &pass_ctx(&dir, 1024),
            &mut CullTable::default(),
            files(2),
            &["Da"],
        );

        assert_eq!(stats.culled, 2);
        assert_eq!(stats.skipped_busy, 1);
        assert_eq!(remaining(&bucket), vec!["Da", "Dd"]);
    }

    #[test]
    fn pass_ends_when_everything_left_is_busy() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10), ("Db", 20)]);

        let target = CullTarget {
            bytes: u64::MAX,
            files: 0,
        };
        let (stats, asked) = pass(
            &pass_ctx(&dir, 1024),
            &mut CullTable::default(),
            target,
            &["Da", "Db"],
        );

        assert!(!stats.made_progress());
        assert_eq!(stats.skipped_busy, 2);
        assert_eq!(asked, vec!["Da", "Db"], "each busy object asked once");
        assert_eq!(remaining(&bucket), vec!["Da", "Db"]);
    }

    #[test]
    fn pass_with_zero_target_culls_nothing() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10)]);

        let (stats, _) = pass(
            &pass_ctx(&dir, 1024),
            &mut CullTable::default(),
            CullTarget::default(),
            &[],
        );

        assert_eq!(stats.culled, 0);
        assert_eq!(stats.candidates, 0);
        assert_eq!(stats.walks, 0);
        assert_eq!(remaining(&bucket), vec!["Da"]);
    }

    #[test]
    fn pass_counts_graveyard_toward_target() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10)]);
        let graveyard = dir.join("graveyard");
        fs::create_dir_all(&graveyard).unwrap();
        touch(&graveyard.join("dead"), 0);
        let in_graveyard = allocated_of(&graveyard, &["dead"]);

        let target = CullTarget {
            bytes: in_graveyard,
            files: 1,
        };
        let (stats, _) = pass(
            &pass_ctx(&dir, 1024),
            &mut CullTable::default(),
            target,
            &[],
        );

        assert_eq!(stats.graveyard_removed, 1);
        assert_eq!(stats.culled, 0);
        assert_eq!(remaining(&bucket), vec!["Da"]);
    }

    #[test]
    fn cache_usage_counts_every_object() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10), ("Db", 20)]);
        let usage = cache_usage(&dir, &AtomicBool::new(false), &Watchdog::disabled()).unwrap();
        assert_eq!(usage.objects, 2);
        assert_eq!(usage.allocated_bytes, allocated_of(&bucket, &["Da", "Db"]));
        assert!(cache_usage(&dir, &AtomicBool::new(true), &Watchdog::disabled()).is_none());
    }

    #[test]
    fn cull_target_is_the_shortfall_to_run() {
        let state = CacheState {
            culling: true,
            frun: 100,
            fcull: 70,
            fstop: 30,
            brun: 1000,
            bcull: 700,
            bstop: 300,
        };
        let low = FreeSpace {
            block_size: 4096,
            blocks_total: 10_000,
            blocks_avail: 600,
            files_total: 1000,
            files_free: 90,
        };
        assert_eq!(
            CullTarget::new(&state, &low),
            CullTarget {
                bytes: 400 * 4096,
                files: 10
            }
        );

        let above_run = FreeSpace {
            blocks_avail: 1200,
            files_free: 500,
            ..low
        };
        assert!(CullTarget::new(&state, &above_run).is_zero());
    }

    #[test]
    fn allocated_bytes_ignore_sparse_holes() {
        // cachefiles sizes each cache file to the whole remote file but
        // only stores the ranges that were read, so st_size can be many
        // times what culling the file gives back.
        let dir = tempdir();
        let path = dir.join("Dsparse");
        fs::File::create(&path).unwrap().set_len(1 << 30).unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), 1 << 30);
        assert!(allocated_bytes(&meta) < meta.len());
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "nfs-cachefs-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }
}
