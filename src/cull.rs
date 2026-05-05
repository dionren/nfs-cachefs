//! Cache cull driver. Walks `<cache_dir>/cache`, picks the K oldest-by-
//! atime files via a streaming top-K, and sends `cull <name>` commands
//! to the kernel one at a time.
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
/// oldest-by-atime files, and culls them in atime-ascending order.
///
/// Per-object errors (EBUSY, chdir failure, etc.) are logged and counted
/// but never propagated: the kernel re-signals if more culling is needed,
/// and a single bad object should not bring the daemon down.
pub fn run_pass(dev: &Device, ctx: &CullCtx) {
    let started = std::time::Instant::now();
    let cache_subdir = ctx.cache_root.join("cache");
    if !cache_subdir.exists() {
        warn!(
            path = %cache_subdir.display(),
            "cache subdir does not exist; kernel layout changed or bind never created it"
        );
        return;
    }

    let oldest = collect_oldest(&cache_subdir, ctx.batch_size);
    debug!(found = oldest.len(), "cull candidates");
    if oldest.is_empty() {
        return;
    }

    let saved_cwd = std::env::current_dir().ok();
    let mut culled = 0usize;
    let mut bytes_freed: u64 = 0;
    let mut skipped_busy = 0usize;
    let mut errored = 0usize;

    for cand in oldest {
        if let Err(e) = std::env::set_current_dir(&cand.parent) {
            warn!(parent = %cand.parent.display(), error = %e, "chdir failed; skip");
            errored += 1;
            continue;
        }
        match cmd::cull(dev, &cand.name) {
            Ok(true) => {
                culled += 1;
                bytes_freed += cand.size;
            }
            Ok(false) => {
                skipped_busy += 1;
            }
            Err(e) => {
                warn!(name = cand.name, error = %e, "cull failed");
                errored += 1;
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
        culled,
        bytes_freed,
        skipped_busy,
        errored,
        "cull pass done"
    );
}

/// Walk the cache subtree and return the `k` oldest-by-atime files in
/// ascending-atime order. Uses a max-heap of size k → O(N log k) time,
/// O(k) memory regardless of cache size.
fn collect_oldest(root: &Path, k: usize) -> Vec<Candidate> {
    if k == 0 {
        return Vec::new();
    }
    let mut heap: BinaryHeap<Candidate> = BinaryHeap::with_capacity(k);

    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .same_file_system(true)
        .into_iter()
        .filter_map(|r| r.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
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

    #[test]
    fn top_k_returns_k_oldest_in_ascending_order() {
        let dir = tempdir();
        for (n, off) in [("a", 100), ("b", 50), ("c", 200), ("d", 25), ("e", 75)] {
            touch(&dir.join(n), off);
        }
        let oldest = collect_oldest(&dir, 3);
        let names: Vec<_> = oldest.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["d", "b", "e"]);
    }

    #[test]
    fn top_k_handles_k_greater_than_n() {
        let dir = tempdir();
        touch(&dir.join("a"), 10);
        touch(&dir.join("b"), 20);
        let oldest = collect_oldest(&dir, 100);
        assert_eq!(oldest.len(), 2);
        assert_eq!(oldest[0].name, "a");
        assert_eq!(oldest[1].name, "b");
    }

    #[test]
    fn top_k_zero_returns_empty() {
        let dir = tempdir();
        touch(&dir.join("a"), 10);
        assert!(collect_oldest(&dir, 0).is_empty());
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
