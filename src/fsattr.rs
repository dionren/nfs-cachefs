//! `FS_IOC_FSGETXATTR` / `FS_IOC_FSSETXATTR`: the project ID, the
//! project-inherit flag and the extent size hint of a file or directory.
//! XFS supports all three, ext4 the project ID.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};

/// `struct fsxattr` from `<linux/fs.h>`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FsXattr {
    pub xflags: u32,
    pub extsize: u32,
    pub nextents: u32,
    pub projid: u32,
    pub cowextsize: u32,
    pub pad: [u8; 8],
}

/// `_IOR('X', 31, struct fsxattr)` / `_IOW('X', 32, struct fsxattr)`.
const FS_IOC_FSGETXATTR: u32 = 0x801c_581f;
const FS_IOC_FSSETXATTR: u32 = 0x401c_5820;

pub(crate) const FS_XFLAG_PROJINHERIT: u32 = 0x0000_0200;
pub(crate) const FS_XFLAG_EXTSZINHERIT: u32 = 0x0000_1000;

/// Open `path` for the attribute ioctls without following a final symlink.
pub(crate) fn open(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

pub(crate) fn get(file: &File) -> io::Result<FsXattr> {
    let mut attr = FsXattr::default();
    // SAFETY: FS_IOC_FSGETXATTR writes one `struct fsxattr`, which FsXattr
    // mirrors field for field.
    let r = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            FS_IOC_FSGETXATTR as _,
            &mut attr as *mut FsXattr,
        )
    };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(attr)
}

pub(crate) fn set(file: &File, attr: &FsXattr) -> io::Result<()> {
    // SAFETY: FS_IOC_FSSETXATTR reads one `struct fsxattr`.
    let r = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            FS_IOC_FSSETXATTR as _,
            attr as *const FsXattr,
        )
    };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Give `root` and every directory under it the XFS extent size hint
/// `extsize` with the inherit flag. Directories the kernel creates later
/// inherit both, and so do the cache files it creates in them, which XFS
/// then allocates in `extsize`-aligned chunks instead of one extent per
/// out-of-order write. Existing files keep their layout. Returns how many
/// directories changed.
pub fn apply_extsize_hint(root: &Path, extsize: u32, stop: &AtomicBool) -> Result<u64> {
    let mut changed = 0;
    let dirs = walkdir::WalkDir::new(root)
        .follow_links(false)
        .same_file_system(true)
        .into_iter()
        .filter_entry(|e| e.file_type().is_dir());
    for entry in dirs {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Interrupted);
        }
        let entry = entry.map_err(|e| Error::Io(io::Error::other(e.to_string())))?;
        let path = entry.path();
        let fail = |what: &str, e: io::Error| {
            let hint = match e.raw_os_error() {
                Some(libc::ENOTTY | libc::EOPNOTSUPP | libc::EINVAL) => {
                    " (extent size hints need XFS)"
                }
                _ => "",
            };
            Error::config(format!("extsize: {what} {}: {e}{hint}", path.display()))
        };
        let file = open(path).map_err(|e| fail("open", e))?;
        let mut attr = get(&file).map_err(|e| fail("read attributes of", e))?;
        if attr.extsize == extsize && attr.xflags & FS_XFLAG_EXTSZINHERIT != 0 {
            continue;
        }
        attr.extsize = extsize;
        attr.xflags |= FS_XFLAG_EXTSZINHERIT;
        set(&file, &attr).map_err(|e| fail("set the hint on", e))?;
        changed += 1;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_the_kernel() {
        assert_eq!(std::mem::size_of::<FsXattr>(), 28);
        // _IOC(dir, 'X', nr, size): dir in bits 30-31, size in 16-29.
        assert_eq!(FS_IOC_FSGETXATTR, (2 << 30) | (28 << 16) | (0x58 << 8) | 31);
        assert_eq!(FS_IOC_FSSETXATTR, (1 << 30) | (28 << 16) | (0x58 << 8) | 32);
    }

    /// Needs an XFS directory the test user owns:
    /// `NFS_CACHEFS_TEST_XFS_DIR=/mnt/xfs/scratch cargo test -- --ignored`.
    #[test]
    #[ignore = "needs NFS_CACHEFS_TEST_XFS_DIR on XFS"]
    fn extsize_hint_is_inherited_on_xfs() {
        let Some(base) = std::env::var_os("NFS_CACHEFS_TEST_XFS_DIR") else {
            eprintln!("NFS_CACHEFS_TEST_XFS_DIR not set; skipping");
            return;
        };
        let root = Path::new(&base).join(format!("extsize-{}", std::process::id()));
        std::fs::create_dir_all(root.join("cache").join("Ivol")).unwrap();
        let stop = AtomicBool::new(false);

        assert_eq!(apply_extsize_hint(&root, 1 << 20, &stop).unwrap(), 3);
        assert_eq!(apply_extsize_hint(&root, 1 << 20, &stop).unwrap(), 0);

        // New directories and files inherit the hint.
        let fan = root.join("cache").join("Ivol").join("@00");
        std::fs::create_dir(&fan).unwrap();
        let obj = fan.join("Dobj");
        std::fs::write(&obj, b"").unwrap();
        let attr = get(&open(&obj).unwrap()).unwrap();
        assert_eq!(attr.extsize, 1 << 20);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
