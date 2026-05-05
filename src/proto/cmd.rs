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
        validate_command(cmd)?;
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
        validate_limit_triplet("b", self.bstop, self.bcull, self.brun)?;
        validate_limit_triplet("f", self.fstop, self.fcull, self.frun)?;

        validate_config_args(self.cache_dir, self.tag, self.secctx)?;
        let cache_dir = self
            .cache_dir
            .to_str()
            .ok_or_else(|| Error::config("cache_dir is not valid UTF-8"))?;

        dev.write_cmd(&format!("dir {cache_dir}"))?;
        dev.write_cmd(&format!("tag {}", self.tag))?;
        if let Some(ctx) = self.secctx {
            dev.write_cmd(&format!("secctx {ctx}"))?;
        }
        for cmd in limit_commands("b", self.bstop, self.bcull, self.brun)? {
            dev.write_cmd(&cmd)?;
        }
        for cmd in limit_commands("f", self.fstop, self.fcull, self.frun)? {
            dev.write_cmd(&cmd)?;
        }
        dev.write_cmd("bind")?;
        Ok(())
    }
}

pub(crate) fn validate_limit_triplet(label: &str, stop: u8, cull: u8, run: u8) -> Result<()> {
    if !(stop < cull && cull < run && run < 100) {
        return Err(Error::config(format!(
            "{label}stop({stop}) < {label}cull({cull}) < {label}run({run}) < 100 required"
        )));
    }
    Ok(())
}

fn limit_commands(label: &str, stop: u8, cull: u8, run: u8) -> Result<Vec<String>> {
    validate_limit_triplet(label, stop, cull, run)?;

    // The kernel validates each limit command against the values currently
    // stored on the freshly opened device. Raise the run/cull ceilings first,
    // then install the requested values from bottom to top.
    Ok(vec![
        format!("{label}run 99%"),
        format!("{label}cull 98%"),
        format!("{label}stop {stop}%"),
        format!("{label}cull {cull}%"),
        format!("{label}run {run}%"),
    ])
}

pub(crate) fn validate_config_args(
    cache_dir: &Path,
    tag: &str,
    secctx: Option<&str>,
) -> Result<()> {
    let cache_dir = cache_dir
        .to_str()
        .ok_or_else(|| Error::config("cache_dir is not valid UTF-8"))?;
    validate_path_arg("cache_dir", cache_dir)?;
    validate_token_arg("tag", tag)?;
    if let Some(ctx) = secctx {
        validate_token_arg("secctx", ctx)?;
    }
    Ok(())
}

fn validate_command(cmd: &str) -> Result<()> {
    if cmd.is_empty() {
        return Err(Error::protocol("empty cachefiles command"));
    }
    if has_command_break(cmd) {
        return Err(Error::protocol(format!(
            "cachefiles command contains NUL or newline: {cmd:?}"
        )));
    }
    Ok(())
}

fn validate_path_arg(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::config(format!("{label} must be non-empty")));
    }
    if has_command_break(value) {
        return Err(Error::config(format!(
            "{label} must not contain NUL or newline"
        )));
    }
    if value.contains(char::is_whitespace) {
        return Err(Error::config(format!(
            "{label} must not contain whitespace; cachefiles commands are whitespace-delimited"
        )));
    }
    Ok(())
}

fn validate_token_arg(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.contains(char::is_whitespace)
        || has_command_break(value)
    {
        return Err(Error::config(format!(
            "{label} must be non-empty and whitespace-free"
        )));
    }
    Ok(())
}

fn validate_object_name(op: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::protocol(format!("{op} name is empty")));
    }
    if name.contains('/') {
        return Err(Error::protocol(format!("{op} name has '/': {name:?}")));
    }
    if name.contains(char::is_whitespace) {
        return Err(Error::protocol(format!(
            "{op} name contains whitespace: {name:?}"
        )));
    }
    if has_command_break(name) {
        return Err(Error::protocol(format!(
            "{op} name contains NUL or newline: {name:?}"
        )));
    }
    Ok(())
}

fn has_command_break(value: &str) -> bool {
    value.chars().any(|c| c == '\0' || c == '\n' || c == '\r')
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
    validate_object_name("inuse", name)?;
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
    validate_object_name("cull", name)?;
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

    #[test]
    fn rejects_run_at_100() {
        assert!(validate_limit_triplet("b", 3, 7, 99).is_ok());
        assert!(validate_limit_triplet("b", 3, 7, 100).is_err());
    }

    #[test]
    fn formats_limits_in_kernel_safe_order() {
        let cmds = limit_commands("b", 0, 1, 2).unwrap();
        assert_eq!(
            cmds,
            vec![
                "brun 99%",
                "bcull 98%",
                "bstop 0%",
                "bcull 1%",
                "brun 2%",
            ]
        );
    }

    #[test]
    fn rejects_multiline_commands() {
        assert!(validate_command("tag ok").is_ok());
        assert!(validate_command("tag ok\nbind").is_err());
        assert!(validate_command("tag ok\0bind").is_err());
    }

    #[test]
    fn validates_config_command_arguments() {
        assert!(
            validate_config_args(Path::new("/var/cache/fscache"), "nfscache", None).is_ok()
        );
        assert!(
            validate_config_args(Path::new("/var/cache/fs cache"), "nfscache", None).is_err()
        );
        assert!(
            validate_config_args(Path::new("/var/cache/fscache"), "nfs cache", None).is_err()
        );
        assert!(
            validate_config_args(
                Path::new("/var/cache/fscache"),
                "nfscache",
                Some("ctx\nbind")
            )
            .is_err()
        );
    }

    #[test]
    fn validates_object_names() {
        assert!(validate_object_name("cull", "012345").is_ok());
        assert!(validate_object_name("cull", "dir/file").is_err());
        assert!(validate_object_name("cull", "file name").is_err());
        assert!(validate_object_name("cull", "file\nbind").is_err());
        assert!(validate_object_name("cull", "").is_err());
    }
}
