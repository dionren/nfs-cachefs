//! Cache cull driver. Walks the cache directory, picks LRU candidates by
//! atime, and sends `cull <name>` commands to the kernel. Honors the
//! kernel's per-command CWD requirement: before each cull, the daemon's
//! working directory must be the parent of the object.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::error::Result;
use crate::proto::{cmd, Device};

/// Configurable knobs for one cull pass.
#[derive(Debug, Clone)]
pub struct CullCtx {
    pub cache_root: PathBuf,
    pub batch_size: usize,
}

#[derive(Debug)]
struct Candidate {
    parent: PathBuf,
    name: String,
    atime_secs: i64,
    size: u64,
}

/// Run one cull pass. Walks the cache root, sorts files by atime ascending,
/// and culls up to `batch_size` of them (or until kernel says "all good").
///
/// Returns when the batch is exhausted; the daemon's main loop will re-poll
/// state and call us again if culling is still needed.
pub fn run_pass(dev: &Device, ctx: &CullCtx) -> Result<()> {
    let started = std::time::Instant::now();
    let cache_subdir = ctx.cache_root.join("cache");
    if !cache_subdir.exists() {
        debug!(path = %cache_subdir.display(), "cache subdir does not exist yet; nothing to cull");
        return Ok(());
    }

    let mut candidates = Vec::with_capacity(1024);
    collect_candidates(&cache_subdir, &mut candidates);
    debug!(found = candidates.len(), "cull candidates");
    if candidates.is_empty() {
        return Ok(());
    }

    candidates.sort_unstable_by_key(|c| c.atime_secs);

    let saved_cwd = std::env::current_dir().ok();
    let mut culled = 0usize;
    let mut bytes_freed: u64 = 0;
    let mut skipped_busy = 0usize;
    let mut errored = 0usize;

    for cand in candidates.iter().take(ctx.batch_size) {
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
        let _ = std::env::set_current_dir(cwd);
    }

    info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        candidates = candidates.len(),
        culled,
        bytes_freed,
        skipped_busy,
        errored,
        "cull pass done"
    );
    Ok(())
}

fn collect_candidates(root: &Path, out: &mut Vec<Candidate>) {
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .same_file_system(true)
        .into_iter()
        .filter_map(|r| r.ok())
    {
        let path = entry.path();
        let ft = entry.file_type();
        if !ft.is_file() {
            continue;
        }
        // cachefiles uses object names that may contain commas, NULs, etc.,
        // but path traversal here is opaque to us — we only need basename
        // and parent.
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(parent) = path.parent() else { continue };
        let Ok(meta) = fs::metadata(path) else { continue };
        out.push(Candidate {
            parent: parent.to_path_buf(),
            name: name.to_string(),
            atime_secs: meta.atime(),
            size: meta.len(),
        });
    }
}
