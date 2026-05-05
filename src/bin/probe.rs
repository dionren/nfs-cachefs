//! Diagnostic probe for the traditional cachefiles control path.
//!
//! Opens /dev/cachefiles, applies the same configuration commands as the
//! daemon via `ConfigCmd::apply_and_bind`, logs every state line the
//! kernel emits, and exits on SIGTERM/SIGINT/SIGHUP. It intentionally
//! does not cull objects; use the full daemon for that.

use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use tracing::{error, info, warn};

use nfs_cachefs::proto::{CacheState, ConfigCmd, Device};
use nfs_cachefs::signals;

#[derive(Parser, Debug)]
#[command(version, about = "Diagnostic probe for Linux cachefiles traditional mode")]
struct Args {
    /// Cache directory on NVMe (must be a dedicated mountpoint).
    #[arg(long, default_value = "/var/cache/fscache")]
    cache_dir: PathBuf,

    /// Cache tag identifier (must be unique among bound caches).
    #[arg(long, default_value = "nfscache-probe")]
    tag: String,

    /// Free space % to resume caching (brun > bcull > bstop).
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u8).range(0..=100))]
    brun: u8,
    /// Free space % at which to start culling.
    #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u8).range(0..=100))]
    bcull: u8,
    /// Free space % at which to refuse new opens.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(0..=100))]
    bstop: u8,

    /// Free inode % to resume caching.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u8).range(0..=100))]
    frun: u8,
    /// Free inode % to start culling.
    #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u8).range(0..=100))]
    fcull: u8,
    /// Free inode % at which to refuse new opens.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(0..=100))]
    fstop: u8,
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: i32) {
    STOP.store(true, Ordering::Relaxed);
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .with_target(false)
        .init();

    if let Err(e) = run() {
        error!("probe failed: {:#}", e);
        process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    use anyhow::Context;

    let args = Args::parse();
    signals::install(handle_signal).context("install signal handlers")?;

    let dev = Device::open().context("open /dev/cachefiles (need CAP_SYS_ADMIN)")?;
    info!(fd = dev.as_raw_fd(), "opened /dev/cachefiles");

    let cmd = ConfigCmd {
        cache_dir: &args.cache_dir,
        tag: &args.tag,
        secctx: None,
        brun: args.brun,
        bcull: args.bcull,
        bstop: args.bstop,
        frun: args.frun,
        fcull: args.fcull,
        fstop: args.fstop,
    };
    cmd.apply_and_bind(&dev)
        .context("apply_and_bind (cache_dir must be its own mountpoint with xattr support)")?;
    info!("bound cachefiles in traditional mode");

    let mut buf = [0u8; 256];
    while !STOP.load(Ordering::Relaxed) {
        let mut pollfd = libc::pollfd {
            fd: dev.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pollfd, 1, 1000) };
        if r < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            anyhow::bail!("poll: {err}");
        }
        if r == 0 || (pollfd.revents & libc::POLLIN) == 0 {
            continue;
        }
        match dev.read_state(&mut buf) {
            Ok(0) => {}
            Ok(n) => match CacheState::parse(&buf[..n]) {
                Ok(state) => info!(?state, "state"),
                Err(e) => warn!(error = %e, "parse state"),
            },
            Err(e) => warn!(error = %e, "read /dev/cachefiles"),
        }
    }

    info!("stop signal received; closing /dev/cachefiles");
    Ok(())
}
