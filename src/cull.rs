//! Cache cull driver. Walks `<cache_dir>/cache/<volume>/@<bucket>/`, picks
//! the K oldest-by-atime cookie objects via a streaming top-K, and sends
//! `cull <name>` commands to the kernel one at a time until the pass has
//! freed what the kernel asked for (see "How much to cull").
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
//! Before each cull we therefore `chdir(parent)`. This is the reason
//! cull is single-threaded: `chdir(2)` mutates a struct that, depending
//! on how threads were created, can be shared between threads. CLAUDE.md
//! flags single-threaded cull as a design invariant; calling `run_pass`
//! concurrently would race even within this process.
//!
//! ## How much to cull
//!
//! The kernel sets `cull=1` when free space drops below `bcull`/`fcull`
//! and clears it once free space is back at `brun`/`frun`. A pass is sized
//! by a [`CullTarget`]: the bytes and inodes missing to reach `brun`/`frun`
//! according to `statfs(cache_dir)`, the same numbers the kernel compares.
//! The pass culls oldest-first and stops as soon as the target is met,
//! walking the cache again if one batch of K is not enough. Freed space is
//! counted in allocated bytes (`st_blocks`), not `st_size`: cache files are
//! sparse, often several times larger than what they occupy.
//!
//! Freed space can take a while to show up in statfs (XFS reclaims
//! unlinked inodes in background workers) and the kernel keeps `cull=1`
//! until it does, so the daemon waits for it before sizing another pass.
//! See `daemon.rs`.
//!
//! ## Memory
//!
//! O(K) regardless of cache size, where K = `batch_size`. A million-
//! object cache with the default batch_size=1024 uses ~64 KiB of heap
//! state during a pass — it never builds an N-sized vector.
//!
//! ## Symlinks
//!
//! `walkdir` is configured with `follow_links(false)` and `entry.metadata()`
//! follows the same policy (uses `symlink_metadata` under the hood). The
//! kernel cachefiles backend never creates symlinks in the cache, so this
//! is defense-in-depth.

use std::collections::BinaryHeap;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::proto::{cmd, CacheState, Device};

/// Configurable knobs for one cull pass.
#[derive(Debug, Clone)]
pub struct CullCtx {
    pub cache_root: PathBuf,
    pub batch_size: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CullStats {
    pub candidates: usize,
    pub culled: usize,
    /// Allocated bytes (`st_blocks`) of everything removed, graveyard
    /// included. Not `st_size`: cache files are sparse.
    pub bytes_freed: u64,
    /// Inodes removed: one per culled object, plus graveyard contents.
    pub files_freed: u64,
    pub skipped_busy: usize,
    pub skipped_changed: usize,
    pub errored: usize,
    pub graveyard_removed: usize,
}

impl CullStats {
    pub fn made_progress(self) -> bool {
        self.culled > 0 || self.graveyard_removed > 0
    }
}

/// Free space on the cache filesystem in the units of the kernel's limits:
/// cachefiles compares `f_bavail` and `f_ffree` from `statfs(cache_dir)`
/// against the `b*` and `f*` values it reports in the state line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeSpace {
    /// Bytes per block of `blocks_avail` (statvfs `f_frsize`).
    pub block_size: u64,
    pub blocks_avail: u64,
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
            blocks_avail: u64::from(st.blocks_available()),
            files_free: u64::from(st.files_free()),
        })
    }
}

/// What a cull pass must free for the kernel to clear `cull=1`: the bytes
/// and inodes missing to bring free space back up to `brun`/`frun`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CullTarget {
    pub bytes: u64,
    pub files: u64,
}

impl CullTarget {
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

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Candidate {
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
    parent: PathBuf,
    name: String,
}

impl Candidate {
    fn path(&self) -> PathBuf {
        self.parent.join(&self.name)
    }

    fn atime_changed(&self) -> std::io::Result<bool> {
        let meta = std::fs::symlink_metadata(self.path())?;
        Ok(meta.atime() != self.atime_secs || meta.atime_nsec() != self.atime_nsecs)
    }

    fn is_older_than(&self, other: &Self) -> bool {
        (self.atime_secs, self.atime_nsecs) < (other.atime_secs, other.atime_nsecs)
    }
}

/// Run one cull pass: cull oldest-by-atime objects until `target` has been
/// freed. Objects are collected `batch_size` at a time and the cache is
/// walked again if one batch is not enough. The pass also ends on stop, or
/// when a whole batch frees nothing (everything left is busy or failing).
///
/// Per-object errors (EBUSY, chdir failure, etc.) are logged and counted
/// but never propagated: the kernel re-signals if more culling is needed,
/// and a single bad object should not bring the daemon down.
pub fn run_pass(dev: &Device, ctx: &CullCtx, target: CullTarget, stop: &AtomicBool) -> CullStats {
    let saved_cwd = std::env::current_dir().ok();
    let stats = run_pass_with(ctx, target, stop, |cand| {
        std::env::set_current_dir(&cand.parent).map_err(|e| {
            Error::Io(io::Error::new(
                e.kind(),
                format!("chdir {}: {e}", cand.parent.display()),
            ))
        })?;
        cmd::cull(dev, &cand.name)
    });
    if let Some(cwd) = saved_cwd {
        if let Err(e) = std::env::set_current_dir(&cwd) {
            warn!(cwd = %cwd.display(), error = %e, "failed to restore cwd");
        }
    }
    stats
}

/// [`run_pass`] with the kernel `cull` command passed in, so the pass logic
/// can be tested without `/dev/cachefiles`. `cull_one` returns `Ok(false)`
/// when the kernel reports the object busy.
fn run_pass_with(
    ctx: &CullCtx,
    target: CullTarget,
    stop: &AtomicBool,
    mut cull_one: impl FnMut(&Candidate) -> Result<bool>,
) -> CullStats {
    let started = std::time::Instant::now();
    let mut stats = clean_graveyard_interruptible(&ctx.cache_root, Some(stop));
    let cache_subdir = ctx.cache_root.join("cache");
    if !cache_subdir.exists() {
        warn!(
            path = %cache_subdir.display(),
            "cache subdir does not exist; kernel layout changed or bind never created it"
        );
        return stats;
    }

    while !target.met_by(&stats) && !stop.load(Ordering::Relaxed) {
        let oldest = collect_oldest_interruptible(&cache_subdir, ctx.batch_size, Some(stop));
        debug!(found = oldest.len(), "cull candidates");
        stats.candidates += oldest.len();
        let culled_before = stats.culled;

        for cand in oldest {
            if target.met_by(&stats) || stop.load(Ordering::Relaxed) {
                break;
            }
            match cand.atime_changed() {
                Ok(false) => {}
                Ok(true) => {
                    stats.skipped_changed += 1;
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(e) => {
                    warn!(path = %cand.path().display(), error = %e, "metadata recheck failed; skip");
                    stats.errored += 1;
                    continue;
                }
            }
            match cull_one(&cand) {
                Ok(true) => {
                    stats.culled += 1;
                    stats.bytes_freed += cand.allocated;
                    stats.files_freed += 1;
                }
                Ok(false) => {
                    stats.skipped_busy += 1;
                }
                Err(e) => {
                    warn!(name = cand.name, error = %e, "cull failed");
                    stats.errored += 1;
                }
            }
        }

        // A whole batch freed nothing: what is left is busy, recently used
        // or failing, and walking again would find the same objects.
        if stats.culled == culled_before {
            break;
        }
    }

    info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        target_bytes = target.bytes,
        target_files = target.files,
        culled = stats.culled,
        bytes_freed = stats.bytes_freed,
        skipped_busy = stats.skipped_busy,
        skipped_changed = stats.skipped_changed,
        errored = stats.errored,
        graveyard_removed = stats.graveyard_removed,
        "cull pass done"
    );
    stats
}

/// Walk the cache subtree and return the `k` oldest-by-atime objects in
/// ascending-atime order. Uses a max-heap of size k → O(N log k) time,
/// O(k) memory regardless of cache size.
#[cfg(test)]
fn collect_oldest(root: &Path, k: usize) -> Vec<Candidate> {
    collect_oldest_interruptible(root, k, None)
}

fn collect_oldest_interruptible(
    root: &Path,
    k: usize,
    stop: Option<&AtomicBool>,
) -> Vec<Candidate> {
    if k == 0 {
        return Vec::new();
    }
    let mut heap: BinaryHeap<Candidate> = BinaryHeap::with_capacity(k);

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
            break;
        }
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
        let cand = Candidate {
            atime_secs: meta.atime(),
            atime_nsecs: meta.atime_nsec(),
            allocated: allocated_bytes(&meta),
            parent: parent.to_path_buf(),
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
    heap.into_sorted_vec()
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

pub fn clean_graveyard(cache_root: &Path) -> CullStats {
    clean_graveyard_interruptible(cache_root, None)
}

fn clean_graveyard_interruptible(cache_root: &Path, stop: Option<&AtomicBool>) -> CullStats {
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
        // Measured before removal so a cull pass can count it toward its
        // target.
        let (bytes, files) = allocated_usage(&path);
        let remove_result = match entry.file_type() {
            Ok(ft) if ft.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(e) => Err(e),
        };
        match remove_result {
            Ok(()) => {
                stats.graveyard_removed += 1;
                stats.bytes_freed += bytes;
                stats.files_freed += files;
            }
            Err(e) => {
                stats.errored += 1;
                warn!(path = %path.display(), error = %e, "failed to remove graveyard entry");
            }
        }
    }
    stats
}

/// Allocated bytes and inode count of `path`, including the whole subtree
/// when it is a directory.
fn allocated_usage(path: &Path) -> (u64, u64) {
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .same_file_system(true)
        .into_iter()
        .filter_map(|entry| entry.ok()?.metadata().ok())
        .fold((0, 0), |(bytes, files), meta| {
            (bytes + allocated_bytes(&meta), files + 1)
        })
}

/// Space an object occupies on disk. `st_blocks` counts 512-byte units
/// whatever the filesystem block size is.
fn allocated_bytes(meta: &std::fs::Metadata) -> u64 {
    meta.blocks().saturating_mul(512)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

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

    #[test]
    fn top_k_returns_k_oldest_in_ascending_order() {
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        for (n, off) in [("Da", 100), ("Db", 50), ("Dc", 200), ("Dd", 25), ("De", 75)] {
            touch(&bucket.join(n), off);
        }
        let oldest = collect_oldest(&dir, 3);
        let names: Vec<_> = oldest.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Dd", "Db", "De"]);
    }

    #[test]
    fn top_k_handles_k_greater_than_n() {
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        touch(&bucket.join("Da"), 10);
        touch(&bucket.join("Db"), 20);
        let oldest = collect_oldest(&dir, 100);
        assert_eq!(oldest.len(), 2);
        assert_eq!(oldest[0].name, "Da");
        assert_eq!(oldest[1].name, "Db");
    }

    #[test]
    fn top_k_zero_returns_empty() {
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        touch(&bucket.join("Da"), 10);
        assert!(collect_oldest(&dir, 0).is_empty());
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

        let oldest = collect_oldest(&dir, 10);
        let names: Vec<_> = oldest.iter().map(|c| c.name.as_str()).collect();
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

        let oldest = collect_oldest(&dir, 10);
        let mut names: Vec<_> = oldest.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["Iindex", "Scookie"]);
    }

    #[test]
    fn graveyard_cleanup_removes_entries() {
        let dir = tempdir();
        let graveyard = dir.join("graveyard");
        fs::create_dir_all(graveyard.join("dead-dir")).unwrap();
        fs::write(graveyard.join("dead-dir").join("child"), b"x").unwrap();
        fs::write(graveyard.join("dead-file"), b"x").unwrap();
        let (dir_bytes, dir_files) = allocated_usage(&graveyard.join("dead-dir"));
        let (file_bytes, _) = allocated_usage(&graveyard.join("dead-file"));

        let stats = clean_graveyard(&dir);
        assert_eq!(stats.graveyard_removed, 2);
        assert_eq!(dir_files, 2);
        assert_eq!(stats.files_freed, 3);
        assert_eq!(stats.bytes_freed, dir_bytes + file_bytes);
        assert!(fs::read_dir(&graveyard).unwrap().next().is_none());
    }

    #[test]
    fn candidate_recheck_detects_atime_change() {
        let dir = tempdir();
        let bucket = cookie_bucket(&dir);
        let path = bucket.join("Scookie");
        touch(&path, 10);

        let oldest = collect_oldest(&dir, 1);
        assert_eq!(oldest.len(), 1);
        assert!(!oldest[0].atime_changed().unwrap());

        touch(&path, 20);
        assert!(oldest[0].atime_changed().unwrap());
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
    /// as held.
    fn fake_cull<'a>(busy: &'a [&'a str]) -> impl FnMut(&Candidate) -> Result<bool> + 'a {
        move |cand: &Candidate| {
            if busy.contains(&cand.name.as_str()) {
                return Ok(false);
            }
            fs::remove_file(cand.path())?;
            Ok(true)
        }
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
        let stop = AtomicBool::new(false);
        let stats = run_pass_with(&pass_ctx(&dir, 1024), target, &stop, fake_cull(&[]));

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

        let target = CullTarget { bytes: 0, files: 3 };
        let stop = AtomicBool::new(false);
        let stats = run_pass_with(&pass_ctx(&dir, 2), target, &stop, fake_cull(&[]));

        assert_eq!(stats.culled, 3);
        assert_eq!(stats.candidates, 4, "two walks of batch_size 2");
        assert_eq!(remaining(&bucket), vec!["Dd", "De"]);
    }

    #[test]
    fn pass_skips_busy_objects() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10), ("Db", 20), ("Dc", 30), ("Dd", 40)]);

        let target = CullTarget { bytes: 0, files: 2 };
        let stop = AtomicBool::new(false);
        let stats = run_pass_with(&pass_ctx(&dir, 1024), target, &stop, fake_cull(&["Da"]));

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
        let stop = AtomicBool::new(false);
        let busy = ["Da", "Db"];
        let stats = run_pass_with(&pass_ctx(&dir, 1024), target, &stop, fake_cull(&busy));

        assert!(!stats.made_progress());
        assert_eq!(stats.skipped_busy, 2);
        assert_eq!(remaining(&bucket), vec!["Da", "Db"]);
    }

    #[test]
    fn pass_with_zero_target_culls_nothing() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10)]);

        let stop = AtomicBool::new(false);
        let ctx = pass_ctx(&dir, 1024);
        let stats = run_pass_with(&ctx, CullTarget::default(), &stop, fake_cull(&[]));

        assert_eq!(stats.culled, 0);
        assert_eq!(stats.candidates, 0);
        assert_eq!(remaining(&bucket), vec!["Da"]);
    }

    #[test]
    fn pass_counts_graveyard_toward_target() {
        let dir = tempdir();
        let bucket = cache_with(&dir, &[("Da", 10)]);
        let graveyard = dir.join("graveyard");
        fs::create_dir_all(&graveyard).unwrap();
        touch(&graveyard.join("dead"), 0);
        let (in_graveyard, _) = allocated_usage(&graveyard.join("dead"));

        let target = CullTarget {
            bytes: in_graveyard,
            files: 1,
        };
        let stop = AtomicBool::new(false);
        let stats = run_pass_with(&pass_ctx(&dir, 1024), target, &stop, fake_cull(&[]));

        assert_eq!(stats.graveyard_removed, 1);
        assert_eq!(stats.culled, 0);
        assert_eq!(remaining(&bucket), vec!["Da"]);
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
            blocks_avail: 600,
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
            block_size: 4096,
            blocks_avail: 1200,
            files_free: 500,
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
