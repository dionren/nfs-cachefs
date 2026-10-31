//! Cache size budget (`max_size`) as a filesystem project quota.
//!
//! cachefiles has no byte limit of its own: `brun`/`bcull`/`bstop` are
//! percentages of what statfs reports for the cache directory. On XFS and
//! ext4, statfs on a directory that carries a project ID with the inherit
//! flag reports the project's block limit as the filesystem size, and the
//! smaller of "limit minus usage" and the real free space as available.
//! Tagging the cache tree with a project and limiting that project
//! therefore turns the percentages into percentages of the budget, while
//! the cache still gives space back when a shared filesystem fills up.
//! This relies on `cache_dir` being its own mount (the self-bind): the
//! kernel statfs's the root of the mount it was given.
//!
//! The kernel reads the limit at bind, so this runs before bind. The whole
//! tree is tagged before bind too: XFS refuses renames between directories
//! of different projects with `EXDEV`, and cachefiles treats a failed
//! rename into its graveyard as an I/O error that stops the cache.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::warn;

use crate::cull::FreeSpace;
use crate::error::{Error, Result};
use crate::fsattr::{self, FS_XFLAG_PROJINHERIT};
use crate::util::human_bytes;

/// `quotactl_fd(2)`, Linux 5.14+. Syscalls added since 5.1 share one
/// number on every architecture (alpha aside).
const SYS_QUOTACTL_FD: libc::c_long = 443;

const PRJQUOTA: u32 = 2;
const XQM_CMD_BASE: u32 = (b'X' as u32) << 8;
const Q_XGETQUOTA: u32 = XQM_CMD_BASE + 3;
const Q_XSETQLIM: u32 = XQM_CMD_BASE + 4;
const Q_XGETQSTATV: u32 = XQM_CMD_BASE + 8;

const fn qcmd(cmd: u32, kind: u32) -> u32 {
    (cmd << 8) | (kind & 0xff)
}

const FS_DQUOT_VERSION: i8 = 1;
const FS_PROJ_QUOTA: i8 = 2;
const FS_DQ_BSOFT: u16 = 1 << 2;
const FS_DQ_BHARD: u16 = 1 << 3;
const FS_QSTATV_VERSION1: i8 = 1;
const FS_QUOTA_PDQ_ACCT: u16 = 1 << 4;
const FS_QUOTA_PDQ_ENFD: u16 = 1 << 5;

/// `struct fs_disk_quota` from `<linux/dqblk_xfs.h>`. Block counts are in
/// 512-byte units.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct FsDiskQuota {
    d_version: i8,
    d_flags: i8,
    d_fieldmask: u16,
    d_id: u32,
    d_blk_hardlimit: u64,
    d_blk_softlimit: u64,
    d_ino_hardlimit: u64,
    d_ino_softlimit: u64,
    d_bcount: u64,
    d_icount: u64,
    d_itimer: i32,
    d_btimer: i32,
    d_iwarns: u16,
    d_bwarns: u16,
    d_itimer_hi: i8,
    d_btimer_hi: i8,
    d_rtbtimer_hi: i8,
    d_padding2: i8,
    d_rtb_hardlimit: u64,
    d_rtb_softlimit: u64,
    d_rtbcount: u64,
    d_rtbtimer: i32,
    d_rtbwarns: u16,
    d_padding3: i16,
    d_padding4: [u8; 8],
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct FsQfilestatv {
    qfs_ino: u64,
    qfs_nblks: u64,
    qfs_nextents: u32,
    qfs_pad: u32,
}

/// `struct fs_quota_statv`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct FsQuotaStatv {
    qs_version: i8,
    qs_pad1: u8,
    qs_flags: u16,
    qs_incoredqs: u32,
    qs_uquota: FsQfilestatv,
    qs_gquota: FsQfilestatv,
    qs_pquota: FsQfilestatv,
    qs_btimelimit: i32,
    qs_itimelimit: i32,
    qs_rtbtimelimit: i32,
    qs_bwarnlimit: u16,
    qs_iwarnlimit: u16,
    qs_rtbwarnlimit: u16,
    qs_pad3: u16,
    qs_pad4: u32,
    qs_pad2: [u64; 7],
}

/// A `max_size` budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub project_id: u32,
    pub limit: u64,
}

/// What the project quota reports.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub used_bytes: u64,
    pub inodes: u64,
    /// 0 when the project has no block limit.
    pub limit_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// Files and directories whose project ID or inherit flag changed.
    pub tagged: u64,
    /// Files and directories in the cache tree.
    pub entries: u64,
    pub usage: Usage,
}

/// Put the cache tree under `budget`: check that project quotas are
/// enforced, that the project is not someone else's, tag every file and
/// directory with it, set the block limit, and check that statfs on
/// `cache_root` now reports the budget. Idempotent; `progress` is called
/// with the running entry count during the tagging walk.
pub fn apply(
    cache_root: &Path,
    budget: Budget,
    stop: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<Applied> {
    let root = fsattr::open(cache_root)
        .map_err(|e| Error::config(format!("max_size: open {}: {e}", cache_root.display())))?;
    check_enforced(&root, cache_root)?;

    let attr = fsattr::get(&root).map_err(|e| {
        Error::config(format!(
            "max_size: cannot read the project ID of {}: {e}",
            cache_root.display()
        ))
    })?;
    if attr.projid != 0 && attr.projid != budget.project_id {
        return Err(Error::config(format!(
            "max_size: {} already belongs to project {}; set quota_project_id = {} to keep it, or clear the project first",
            cache_root.display(),
            attr.projid,
            attr.projid
        )));
    }
    if attr.projid != budget.project_id {
        let quota = get_quota(&root, budget.project_id).map_err(|e| quota_error("read", e))?;
        if quota.d_bcount > 0 || quota.d_icount > 0 {
            return Err(Error::config(format!(
                "max_size: project {} already holds {} in {} inodes outside {}; choose another quota_project_id",
                budget.project_id,
                human_bytes(quota.d_bcount * 512),
                quota.d_icount,
                cache_root.display()
            )));
        }
    }

    let (tagged, entries) = tag_tree(cache_root, budget.project_id, stop, &mut progress)?;
    set_limit(&root, budget.project_id, budget.limit).map_err(|e| quota_error("set", e))?;
    verify_statfs(cache_root, budget.limit)?;
    let usage = usage_of(&root, budget.project_id).map_err(|e| quota_error("read", e))?;
    Ok(Applied {
        tagged,
        entries,
        usage,
    })
}

/// The project's usage and limit, for metrics.
pub fn usage(cache_root: &Path, project_id: u32) -> io::Result<Usage> {
    usage_of(&fsattr::open(cache_root)?, project_id)
}

fn usage_of(root: &File, project_id: u32) -> io::Result<Usage> {
    let quota = get_quota(root, project_id)?;
    Ok(Usage {
        used_bytes: quota.d_bcount.saturating_mul(512),
        inodes: quota.d_icount,
        limit_bytes: quota.d_blk_hardlimit.saturating_mul(512),
    })
}

fn quota_error(what: &str, e: io::Error) -> Error {
    Error::config(format!("max_size: cannot {what} the project quota: {e}"))
}

fn quotactl(dir: &File, cmd: u32, id: u32, addr: *mut c_void) -> io::Result<()> {
    // SAFETY: quotactl_fd reads or writes the structure `addr` points to,
    // whose type matches `cmd` at every call site.
    let r = unsafe {
        libc::syscall(
            SYS_QUOTACTL_FD,
            dir.as_raw_fd() as libc::c_long,
            qcmd(cmd, PRJQUOTA) as libc::c_long,
            id as libc::c_long,
            addr,
        )
    };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn check_enforced(root: &File, cache_root: &Path) -> Result<()> {
    let mut statv = FsQuotaStatv {
        qs_version: FS_QSTATV_VERSION1,
        ..FsQuotaStatv::default()
    };
    if let Err(e) = quotactl(root, Q_XGETQSTATV, 0, &mut statv as *mut _ as *mut c_void) {
        return Err(Error::config(if e.raw_os_error() == Some(libc::ENOSYS) {
            "max_size needs quotactl_fd(2), Linux 5.14 or later".to_string()
        } else {
            format!(
                "max_size: cannot read the quota state of the filesystem holding {} ({e}); it must be mounted with project quotas (prjquota)",
                cache_root.display()
            )
        }));
    }
    let wanted = FS_QUOTA_PDQ_ACCT | FS_QUOTA_PDQ_ENFD;
    if statv.qs_flags & wanted != wanted {
        return Err(Error::config(format!(
            "max_size: project quota accounting and enforcement must be on for the filesystem holding {} (mount option prjquota)",
            cache_root.display()
        )));
    }
    Ok(())
}

/// The project's quota record; zeroes when it has none yet.
fn get_quota(root: &File, project_id: u32) -> io::Result<FsDiskQuota> {
    let mut quota = FsDiskQuota::default();
    match quotactl(
        root,
        Q_XGETQUOTA,
        project_id,
        &mut quota as *mut _ as *mut c_void,
    ) {
        Ok(()) => Ok(quota),
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(FsDiskQuota::default()),
        Err(e) => Err(e),
    }
}

fn set_limit(root: &File, project_id: u32, limit: u64) -> io::Result<()> {
    // A zero soft limit makes statfs report the hard limit.
    let mut quota = FsDiskQuota {
        d_version: FS_DQUOT_VERSION,
        d_flags: FS_PROJ_QUOTA,
        d_fieldmask: FS_DQ_BHARD | FS_DQ_BSOFT,
        d_id: project_id,
        d_blk_hardlimit: limit.div_ceil(512),
        d_blk_softlimit: 0,
        ..FsDiskQuota::default()
    };
    quotactl(
        root,
        Q_XSETQLIM,
        project_id,
        &mut quota as *mut _ as *mut c_void,
    )
}

/// Give every file and directory under `cache_root` the project ID, and
/// directories the inherit flag so that what the kernel creates later
/// carries it too. Returns (changed, total).
fn tag_tree(
    cache_root: &Path,
    project_id: u32,
    stop: &AtomicBool,
    progress: &mut impl FnMut(u64),
) -> Result<(u64, u64)> {
    let mut tagged = 0;
    let mut entries = 0;
    let walk = walkdir::WalkDir::new(cache_root)
        .follow_links(false)
        .same_file_system(true);
    for entry in walk {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Interrupted);
        }
        // A tree left half tagged risks EXDEV on renames, which stops the
        // cache: fail rather than bind with it.
        let entry = entry.map_err(|e| {
            Error::config(format!("max_size: walking {}: {e}", cache_root.display()))
        })?;
        let file_type = entry.file_type();
        if !(file_type.is_dir() || file_type.is_file()) {
            continue;
        }
        entries += 1;
        if entries % 1024 == 0 {
            progress(entries);
        }
        let path = entry.path();
        let fail = |what: &str, e: io::Error| {
            let hint = if e.raw_os_error() == Some(libc::EINVAL) && project_id > 0xffff {
                " (the filesystem may lack 32-bit project IDs; use a quota_project_id up to 65535)"
            } else {
                ""
            };
            Error::config(format!(
                "max_size: cannot {what} {}: {e}{hint}",
                path.display()
            ))
        };
        let file = fsattr::open(path).map_err(|e| fail("open", e))?;
        let mut attr = fsattr::get(&file).map_err(|e| fail("read the project ID of", e))?;
        let xflags = if file_type.is_dir() {
            attr.xflags | FS_XFLAG_PROJINHERIT
        } else {
            attr.xflags
        };
        if attr.projid != project_id || attr.xflags != xflags {
            attr.projid = project_id;
            attr.xflags = xflags;
            fsattr::set(&file, &attr).map_err(|e| fail("set the project ID on", e))?;
            tagged += 1;
        }
    }
    Ok((tagged, entries))
}

/// statfs on `cache_root` must now report the budget as its size: XFS
/// reports the limit, ext4 the smaller of the limit and the filesystem.
fn verify_statfs(cache_root: &Path, limit: u64) -> Result<()> {
    let space = FreeSpace::of(cache_root)?;
    let size = space.blocks_total.saturating_mul(space.block_size);
    let slack = 2 * space.block_size;
    if size > limit + slack {
        return Err(Error::config(format!(
            "max_size: statfs on {} still reports {} instead of {}; the project quota does not apply to it",
            cache_root.display(),
            human_bytes(size),
            human_bytes(limit)
        )));
    }
    if size + slack < limit {
        warn!(
            cache_dir = %cache_root.display(),
            fs_size = %human_bytes(size),
            max_size = %human_bytes(limit),
            "max_size exceeds the filesystem; the filesystem size applies"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn layouts_match_the_kernel() {
        assert_eq!(std::mem::size_of::<FsDiskQuota>(), 112);
        assert_eq!(std::mem::size_of::<FsQuotaStatv>(), 160);
        assert_eq!(qcmd(Q_XGETQSTATV, PRJQUOTA), 0x0058_0802);
        assert_eq!(qcmd(Q_XSETQLIM, PRJQUOTA), 0x0058_0402);
    }

    /// Run as root against a filesystem mounted with prjquota:
    /// `sudo NFS_CACHEFS_TEST_PRJQUOTA_DIR=/mnt/xfs <test binary> --ignored quota`.
    #[test]
    #[ignore = "needs root and NFS_CACHEFS_TEST_PRJQUOTA_DIR on a prjquota filesystem"]
    fn applies_a_budget_on_a_real_filesystem() {
        let Some(base) = std::env::var_os("NFS_CACHEFS_TEST_PRJQUOTA_DIR") else {
            eprintln!("NFS_CACHEFS_TEST_PRJQUOTA_DIR not set; skipping");
            return;
        };
        let root = PathBuf::from(base).join(format!("budget-{}", std::process::id()));
        let bucket = root.join("cache").join("Ivol").join("@00");
        std::fs::create_dir_all(&bucket).unwrap();
        std::fs::write(bucket.join("Dold"), vec![b'x'; 1 << 20]).unwrap();
        let stop = AtomicBool::new(false);
        let id = 0x6e66_0000 + (std::process::id() & 0xffff);
        let budget = Budget {
            project_id: id,
            limit: 256 << 20,
        };

        let applied = apply(&root, budget, &stop, |_| {}).unwrap();
        // root, cache, Ivol, @00, Dold
        assert_eq!((applied.entries, applied.tagged), (5, 5));
        assert!(applied.usage.used_bytes >= 1 << 20);
        assert_eq!(applied.usage.limit_bytes, 256 << 20);
        let space = FreeSpace::of(&root).unwrap();
        assert_eq!(space.blocks_total * space.block_size, 256 << 20);

        // What is created later inherits the project.
        std::fs::write(bucket.join("Dnew"), b"y").unwrap();
        let attr = fsattr::get(&fsattr::open(&bucket.join("Dnew")).unwrap()).unwrap();
        assert_eq!(attr.projid, id);

        let again = apply(&root, budget, &stop, |_| {}).unwrap();
        assert_eq!(again.tagged, 0);

        // Another project ID is refused while the tree carries this one.
        let other = Budget {
            project_id: id + 1,
            ..budget
        };
        assert!(apply(&root, other, &stop, |_| {}).is_err());

        let dir = fsattr::open(&root).unwrap();
        set_limit(&dir, id, 0).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }
}
