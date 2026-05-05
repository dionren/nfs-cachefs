//! Cache cull driver. Walks `<cache_dir>/cache/<volume>/@<bucket>/`, picks
//! the K oldest-by-atime cookie objects via a streaming top-K, and sends
//! `cull <name>` commands to the kernel one at a time.
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
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::proto::{cmd, Device};

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
    pub bytes_freed: u64,
    pub skipped_busy: usize,
    pub errored: usize,
    pub graveyard_removed: usize,
}

impl CullStats {
    pub fn made_progress(self) -> bool {
        self.culled > 0 || self.graveyard_removed > 0
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Candidate {
    // atime_secs is the primary sort key. Putting it first makes the
    // derived Ord rank by it, which is what the BinaryHeap relies on:
    // as a max-heap, the root is the YOUNGEST of the K kept so far —
    // the one we should evict if a still-older file shows up.
    atime_secs: i64,
    size: u64,
    parent: PathBuf,
    name: String,
}

/// Run one cull pass. Walks the cache subtree, keeps the `batch_size`
/// oldest-by-atime objects, and culls them in atime-ascending order.
///
/// Per-object errors (EBUSY, chdir failure, etc.) are logged and counted
/// but never propagated: the kernel re-signals if more culling is needed,
/// and a single bad object should not bring the daemon down.
pub fn run_pass(dev: &Device, ctx: &CullCtx) -> CullStats {
    let started = std::time::Instant::now();
    let mut stats = clean_graveyard(&ctx.cache_root);
    let cache_subdir = ctx.cache_root.join("cache");
    if !cache_subdir.exists() {
        warn!(
            path = %cache_subdir.display(),
            "cache subdir does not exist; kernel layout changed or bind never created it"
        );
        return stats;
    }

    let oldest = collect_oldest(&cache_subdir, ctx.batch_size);
    stats.candidates = oldest.len();
    debug!(found = stats.candidates, "cull candidates");
    if oldest.is_empty() {
        return stats;
    }

    let saved_cwd = std::env::current_dir().ok();

    for cand in oldest {
        if let Err(e) = std::env::set_current_dir(&cand.parent) {
            warn!(parent = %cand.parent.display(), error = %e, "chdir failed; skip");
            stats.errored += 1;
            continue;
        }
        match cmd::cull(dev, &cand.name) {
            Ok(true) => {
                stats.culled += 1;
                stats.bytes_freed += cand.size;
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

    if let Some(cwd) = saved_cwd {
        if let Err(e) = std::env::set_current_dir(&cwd) {
            warn!(cwd = %cwd.display(), error = %e, "failed to restore cwd");
        }
    }

    info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        culled = stats.culled,
        bytes_freed = stats.bytes_freed,
        skipped_busy = stats.skipped_busy,
        errored = stats.errored,
        graveyard_removed = stats.graveyard_removed,
        "cull pass done"
    );
    stats
}

/// Walk the cache subtree and return the `k` oldest-by-atime objects in
/// ascending-atime order. Uses a max-heap of size k → O(N log k) time,
/// O(k) memory regardless of cache size.
fn collect_oldest(root: &Path, k: usize) -> Vec<Candidate> {
    if k == 0 {
        return Vec::new();
    }
    let mut heap: BinaryHeap<Candidate> = BinaryHeap::with_capacity(k);

    // Depth 3 = cookie objects (see module docs for the layout). Both
    // bounds are required: min_depth(3) skips the volume index (depth 1)
    // and hash buckets (depth 2); max_depth(3) prevents descending into
    // an index-cookie directory and culling its children individually
    // — culling the parent removes the whole subtree.
    let mut entries = walkdir::WalkDir::new(root)
        .min_depth(3)
        .max_depth(3)
        .follow_links(false)
        .same_file_system(true)
        .into_iter();

    while let Some(entry) = entries.next() {
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
        let Some(parent) = entry.path().parent() else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let cand = Candidate {
            atime_secs: meta.atime(),
            size: meta.len(),
            parent: parent.to_path_buf(),
            name: name.to_string(),
        };
        if heap.len() < k {
            heap.push(cand);
        } else if let Some(top) = heap.peek() {
            // Heap root = youngest of the K so far. If this candidate is
            // older, evict the youngest and keep this one.
            if cand.atime_secs < top.atime_secs {
                heap.pop();
                heap.push(cand);
            }
        }
    }

    // Sorts ascending by Ord (atime_secs first) → oldest first.
    heap.into_sorted_vec()
}

fn is_cache_object_name(name: &str) -> bool {
    matches!(
        name.as_bytes().first(),
        Some(b'I' | b'J' | b'D' | b'E' | b'S' | b'T')
    )
}

fn clean_graveyard(cache_root: &Path) -> CullStats {
    let mut stats = CullStats::default();
    let graveyard = cache_root.join("graveyard");
    let Ok(entries) = std::fs::read_dir(&graveyard) else {
        return stats;
    };

    for entry in entries {
        let Ok(entry) = entry else {
            stats.errored += 1;
            continue;
        };
        let path = entry.path();
        let remove_result = match entry.file_type() {
            Ok(ft) if ft.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(e) => Err(e),
        };
        match remove_result {
            Ok(()) => stats.graveyard_removed += 1,
            Err(e) => {
                stats.errored += 1;
                warn!(path = %path.display(), error = %e, "failed to remove graveyard entry");
            }
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn touch(path: &Path, atime_offset_secs: i64) {
        fs::write(path, b"x").unwrap();
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
        for (n, off) in [
            ("Da", 100),
            ("Db", 50),
            ("Dc", 200),
            ("Dd", 25),
            ("De", 75),
        ] {
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
        fs::write(graveyard.join("dead-file"), b"x").unwrap();

        let stats = clean_graveyard(&dir);
        assert_eq!(stats.graveyard_removed, 2);
        assert!(fs::read_dir(&graveyard).unwrap().next().is_none());
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
