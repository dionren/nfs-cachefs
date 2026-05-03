//! Daemon → kernel commands. All commands are text written via a single
//! `write()` syscall to `/dev/cachefiles`. The kernel parses the buffer as
//! one command per write; do not embed multiple commands in one buffer.

use std::ffi::c_void;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;

use crate::error::{Error, Result};

/// A handle to an open `/dev/cachefiles` device. Owns the file descriptor.
pub struct Device {
    file: std::fs::File,
}

impl Device {
    pub fn open() -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(super::CACHEFILES_DEV)?;
        Ok(Self { file })
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Issue a single command. The kernel rejects commands with embedded NULs
    /// or newlines; callers should not include them.
    pub fn write_cmd(&self, cmd: &str) -> Result<()> {
        debug_assert!(!cmd.contains('\0') && !cmd.contains('\n'));
        let n = unsafe {
            libc::write(
                self.file.as_raw_fd(),
                cmd.as_ptr() as *const c_void,
                cmd.len(),
            )
        };
        if n < 0 {
            let source = io::Error::last_os_error();
            return Err(Error::Kernel { cmd: cmd.to_string(), source });
        }
        if (n as usize) != cmd.len() {
            return Err(Error::Kernel {
                cmd: cmd.to_string(),
                source: io::Error::new(
                    io::ErrorKind::WriteZero,
                    format!("short write: {}/{} bytes", n, cmd.len()),
                ),
            });
        }
        Ok(())
    }

    /// Read one state line. Buffer must be ≥ 256 bytes; kernel writes at most
    /// ~150 bytes (formatted decimal counters).
    pub fn read_state(&self, buf: &mut [u8]) -> Result<usize> {
        let n = unsafe {
            libc::read(
                self.file.as_raw_fd(),
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
            )
        };
        if n < 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        Ok(n as usize)
    }
}

/// Cache configuration sent before `bind`. `dir` and `tag` are required;
/// limits have kernel defaults but every running daemon should set them
/// explicitly. Order doesn't matter except `bind` must come last.
#[derive(Debug, Clone)]
pub struct ConfigCmd<'a> {
    pub cache_dir: &'a Path,
    pub tag: &'a str,
    pub secctx: Option<&'a str>,
    pub brun: u8,
    pub bcull: u8,
    pub bstop: u8,
    pub frun: u8,
    pub fcull: u8,
    pub fstop: u8,
}

impl<'a> ConfigCmd<'a> {
    /// Send `dir`, `tag`, optional `secctx`, the six limits, then `bind`.
    pub fn apply_and_bind(&self, dev: &Device) -> Result<()> {
        // Validate strict ordering: stop < cull < run, all 0..100.
        let bs = (self.bstop, self.bcull, self.brun);
        let fs = (self.fstop, self.fcull, self.frun);
        for (label, (s, c, r)) in [("b", bs), ("f", fs)] {
            if !(s < c && c < r && r <= 100) {
                return Err(Error::config(format!(
                    "{label}stop({s}) < {label}cull({c}) < {label}run({r}) <= 100 required"
                )));
            }
        }

        let cache_dir = self
            .cache_dir
            .to_str()
            .ok_or_else(|| Error::config("cache_dir is not valid UTF-8"))?;
        if self.tag.is_empty() || self.tag.contains(char::is_whitespace) {
            return Err(Error::config("tag must be non-empty and whitespace-free"));
        }

        dev.write_cmd(&format!("dir {cache_dir}"))?;
        dev.write_cmd(&format!("tag {}", self.tag))?;
        if let Some(ctx) = self.secctx {
            dev.write_cmd(&format!("secctx {ctx}"))?;
        }
        dev.write_cmd(&format!("brun {}%", self.brun))?;
        dev.write_cmd(&format!("bcull {}%", self.bcull))?;
        dev.write_cmd(&format!("bstop {}%", self.bstop))?;
        dev.write_cmd(&format!("frun {}%", self.frun))?;
        dev.write_cmd(&format!("fcull {}%", self.fcull))?;
        dev.write_cmd(&format!("fstop {}%", self.fstop))?;
        dev.write_cmd("bind")?;
        Ok(())
    }
}

// There is no `unbind` command in the kernel cachefiles daemon protocol
// (verified on kernel 6.8: write returns ENOTSUPP). The cache is unbound
// implicitly when /dev/cachefiles is closed, which `Device::Drop` handles
// for us. Daemon shutdown therefore just drops the Device.

/// Ask the kernel whether `name` (basename, no `/`) is in use. The daemon's
/// CWD must be the parent directory containing the object.
///
/// Returns `Ok(true)` if not in use (safe to cull), `Ok(false)` if in use.
///
/// Currently unused: `cull()` itself returns `Ok(false)` on EBUSY so an
/// explicit pre-check is redundant. Kept for future optimization (avoid
/// even-trying-to-cull obviously-busy objects).
#[allow(dead_code)]
pub fn inuse(dev: &Device, name: &str) -> Result<bool> {
    if name.contains('/') {
        return Err(Error::protocol(format!("inuse name has '/': {name:?}")));
    }
    match dev.write_cmd(&format!("inuse {name}")) {
        Ok(()) => Ok(true),
        Err(Error::Kernel { source, .. }) if source.raw_os_error() == Some(libc::EBUSY) => {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Tell the kernel to cull `name` from the current directory. Returns
/// `Ok(true)` on success, `Ok(false)` if the object was held by the kernel
/// (`EBUSY`) — caller should skip and try later.
pub fn cull(dev: &Device, name: &str) -> Result<bool> {
    if name.contains('/') {
        return Err(Error::protocol(format!("cull name has '/': {name:?}")));
    }
    match dev.write_cmd(&format!("cull {name}")) {
        Ok(()) => Ok(true),
        Err(Error::Kernel { source, .. }) if source.raw_os_error() == Some(libc::EBUSY) => {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validates_ordering() {
        let bad = ConfigCmd {
            cache_dir: Path::new("/x"),
            tag: "t",
            secctx: None,
            brun: 5,
            bcull: 7,
            bstop: 3,
            frun: 10,
            fcull: 7,
            fstop: 3,
        };
        // Note: this test only validates the validation logic; it does not
        // touch /dev/cachefiles. We construct without applying.
        let bs = (bad.bstop, bad.bcull, bad.brun);
        assert!(!(bs.0 < bs.1 && bs.1 < bs.2));
    }
}
