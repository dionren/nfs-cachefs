//! Traditional cachefiles userspace ↔ kernel protocol over `/dev/cachefiles`.
//!
//! The daemon writes text commands; the kernel returns a single-line state
//! string on each successful `read()`. There is no on-demand event stream
//! on stock Ubuntu 24.04 (`CONFIG_CACHEFILES_ONDEMAND=n`), so the protocol
//! reduces to:
//!
//!   - boot:  open dev → write `dir`/`tag`/`*run`/`*cull`/`*stop` → write `bind`
//!   - run:   poll(dev, POLLIN) → read state line → if `cull=1`, drive cull
//!   - cull:  chdir to object's parent → write `cull <name>`
//!   - exit:  close dev; the kernel withdraws the cache
//!
//! See `Documentation/filesystems/caching/cachefiles.rst` and
//! `fs/cachefiles/daemon.c` in the Linux kernel source.

pub mod cmd;
pub mod state;

pub use cmd::{ConfigCmd, Device};
pub use state::CacheState;

/// Kernel device path. There is exactly one global `/dev/cachefiles`; only
/// one daemon process can hold it open at a time.
pub const CACHEFILES_DEV: &str = "/dev/cachefiles";
