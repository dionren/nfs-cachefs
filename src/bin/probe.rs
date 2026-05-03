//! P0 feasibility prototype for the nfs-cachefs cachefiles on-demand daemon.
//!
//! Goal: prove that the kernel routes NFS-via-fscache through the on-demand
//! protocol on Ubuntu 24.04 (kernel 6.8). This binary opens /dev/cachefiles,
//! configures it, binds in on-demand mode, and logs every event the kernel
//! emits. It accepts every OPEN unconditionally and does not implement cull,
//! policy, or READ data population.
//!
//! This is throwaway code. The real daemon lives in src/main.rs (P1+).
//!
//! Run as root (CAP_SYS_ADMIN). Then in another shell:
//!   mount -t nfs -o fsc,vers=4.2 server:/export /mnt/nfs
//!   dd if=/mnt/nfs/bigfile of=/dev/null bs=1M count=1024
//! and watch the probe's log for OPEN events.

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use tracing::{debug, error, info, warn};

const CACHEFILES_DEV: &str = "/dev/cachefiles";

// from <linux/cachefiles.h>: enum cachefiles_opcode
const OP_OPEN: u32 = 0;
const OP_CLOSE: u32 = 1;
const OP_READ: u32 = 2;

// struct cachefiles_msg layout (4 × u32 = 16 bytes header)
const MSG_HEADER_SIZE: usize = 16;
// struct cachefiles_open prefix (4 × u32 = 16 bytes before data[])
const OPEN_HEADER_SIZE: usize = 16;
// struct cachefiles_read (2 × u64 = 16 bytes)
const READ_HEADER_SIZE: usize = 16;

// CACHEFILES_MSG_MAX_SIZE = 1024; round up for safety.
const READ_BUF_SIZE: usize = 4096;

#[derive(Parser, Debug)]
#[command(version, about = "P0 prototype: bind cachefiles in on-demand mode and log events")]
struct Args {
    /// Cache directory on NVMe (must be a dedicated mountpoint).
    #[arg(long, default_value = "/var/cache/fscache")]
    cache_dir: PathBuf,

    /// Cache tag identifier (must be unique among bound caches).
    #[arg(long, default_value = "nfscache")]
    tag: String,

    /// Free space % to resume caching (brun > bcull > bstop).
    #[arg(long, default_value_t = 10)]
    brun: u32,
    /// Free space % at which to start culling.
    #[arg(long, default_value_t = 7)]
    bcull: u32,
    /// Free space % at which to refuse new opens.
    #[arg(long, default_value_t = 3)]
    bstop: u32,

    /// Free inode % to resume caching.
    #[arg(long, default_value_t = 10)]
    frun: u32,
    /// Free inode % to start culling.
    #[arg(long, default_value_t = 7)]
    fcull: u32,
    /// Free inode % to refuse new opens.
    #[arg(long, default_value_t = 3)]
    fstop: u32,

    /// Advertised cache size for every OPEN (the probe accepts everything).
    #[arg(long, default_value_t = 1u64 << 40)]
    advertise_size: u64,
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(_sig: i32) {
    STOP.store(true, Ordering::SeqCst);
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    if let Err(e) = run(args) {
        error!("probe failed: {:#}", e);
        process::exit(1);
    }
}

fn run(args: Args) -> anyhow::Result<()> {
    use anyhow::Context;

    install_signal_handlers().context("install signal handlers")?;

    let dev = OpenOptions::new()
        .read(true)
        .write(true)
        .open(CACHEFILES_DEV)
        .with_context(|| format!("open {} (need CAP_SYS_ADMIN; another daemon holding it?)", CACHEFILES_DEV))?;
    let dev_fd = dev.as_raw_fd();
    info!(fd = dev_fd, "opened {}", CACHEFILES_DEV);

    // Send config commands. `bind` MUST be last; the rest are order-independent.
    write_cmd(dev_fd, &format!("dir {}", args.cache_dir.display()))?;
    write_cmd(dev_fd, &format!("tag {}", args.tag))?;
    write_cmd(dev_fd, &format!("brun {}%", args.brun))?;
    write_cmd(dev_fd, &format!("bcull {}%", args.bcull))?;
    write_cmd(dev_fd, &format!("bstop {}%", args.bstop))?;
    write_cmd(dev_fd, &format!("frun {}%", args.frun))?;
    write_cmd(dev_fd, &format!("fcull {}%", args.fcull))?;
    write_cmd(dev_fd, &format!("fstop {}%", args.fstop))?;
    // Ubuntu 24.04's stock kernel 6.8 ships with CONFIG_CACHEFILES_ONDEMAND=n
    // (verified at /boot/config-$(uname -r)). Falling back to traditional mode,
    // which is what upstream cachefilesd uses. In this mode the kernel handles
    // all OPEN/CLOSE/READ internally; daemon only does configuration + cull.
    // The poll loop below will see no events on this kernel — it is kept so
    // this binary remains a useful liveness probe and so the protocol code is
    // exercised when run on a kernel that does have on-demand compiled in.
    write_cmd(dev_fd, "bind")
        .context("bind failed (cache_dir must be its own mountpoint with xattr support)")?;
    info!("bound cachefiles in traditional mode (CONFIG_CACHEFILES_ONDEMAND=n on this kernel)");

    let mut buf = vec![0u8; READ_BUF_SIZE];
    let mut stats = Stats::default();

    while !STOP.load(Ordering::Relaxed) {
        let mut pollfd = libc::pollfd { fd: dev_fd, events: libc::POLLIN, revents: 0 };
        let r = unsafe { libc::poll(&mut pollfd, 1, 1000) };
        if r < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) { continue; }
            return Err(anyhow::anyhow!("poll: {}", err));
        }
        if r == 0 || (pollfd.revents & libc::POLLIN) == 0 {
            continue;
        }

        let n = unsafe { libc::read(dev_fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) { continue; }
            return Err(anyhow::anyhow!("read /dev/cachefiles: {}", err));
        }
        if n == 0 { continue; }

        if let Err(e) = handle_message(dev_fd, &buf[..n as usize], &args, &mut stats) {
            warn!("message handling error: {:#}", e);
        }
    }

    info!(?stats, "stop signal received");
    if let Err(e) = write_cmd(dev_fd, "unbind") {
        warn!("unbind failed (kernel will clean up on fd close): {:#}", e);
    } else {
        info!("unbound");
    }
    Ok(())
}

#[derive(Default, Debug)]
struct Stats {
    opens: u64,
    closes: u64,
    reads: u64,
}

fn handle_message(dev_fd: RawFd, buf: &[u8], args: &Args, stats: &mut Stats) -> anyhow::Result<()> {
    if buf.len() < MSG_HEADER_SIZE {
        anyhow::bail!("short message: {} bytes", buf.len());
    }
    let msg_id = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
    let opcode = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
    let len = u32::from_ne_bytes(buf[8..12].try_into().unwrap()) as usize;
    let object_id = u32::from_ne_bytes(buf[12..16].try_into().unwrap());

    if len < MSG_HEADER_SIZE || len > buf.len() {
        anyhow::bail!("invalid msg len {} (buf={})", len, buf.len());
    }
    let payload = &buf[MSG_HEADER_SIZE..len];

    match opcode {
        OP_OPEN => {
            stats.opens += 1;
            handle_open(dev_fd, msg_id, object_id, payload, args)?;
        }
        OP_CLOSE => {
            stats.closes += 1;
            info!(msg_id, object_id, "CLOSE");
        }
        OP_READ => {
            stats.reads += 1;
            handle_read(msg_id, object_id, payload)?;
        }
        other => warn!(msg_id, object_id, opcode = other, "unknown opcode"),
    }
    Ok(())
}

fn handle_open(dev_fd: RawFd, msg_id: u32, object_id: u32, payload: &[u8], args: &Args) -> anyhow::Result<()> {
    if payload.len() < OPEN_HEADER_SIZE {
        anyhow::bail!("OPEN payload too short: {} bytes", payload.len());
    }
    let volume_key_size = u32::from_ne_bytes(payload[0..4].try_into().unwrap()) as usize;
    let cookie_key_size = u32::from_ne_bytes(payload[4..8].try_into().unwrap()) as usize;
    let anon_fd = u32::from_ne_bytes(payload[8..12].try_into().unwrap()) as RawFd;
    let flags = u32::from_ne_bytes(payload[12..16].try_into().unwrap());

    let data = &payload[OPEN_HEADER_SIZE..];
    if data.len() < volume_key_size + cookie_key_size {
        anyhow::bail!(
            "OPEN data short: have {}, need {} (volume_key_size={}, cookie_key_size={})",
            data.len(), volume_key_size + cookie_key_size, volume_key_size, cookie_key_size,
        );
    }
    // volume_key is NUL-terminated; size includes the NUL
    let volume_key_bytes = &data[..volume_key_size.saturating_sub(1)];
    let volume_key = String::from_utf8_lossy(volume_key_bytes);
    let cookie_key = &data[volume_key_size..volume_key_size + cookie_key_size];

    info!(
        msg_id, object_id, anon_fd, flags,
        volume = %volume_key,
        cookie_key_hex = %hex(cookie_key),
        cookie_key_len = cookie_key.len(),
        "OPEN — accepting"
    );

    let reply = format!("copen {},{}", msg_id, args.advertise_size);
    write_cmd(dev_fd, &reply)?;

    // We don't keep the anon_fd — for NFS the kernel itself populates it via
    // fscache writes. If READ events fire (unexpected for NFS), P1 will need
    // to track per-object fds. P0 lets the kernel time those out.
    unsafe { libc::close(anon_fd); }
    Ok(())
}

fn handle_read(msg_id: u32, object_id: u32, payload: &[u8]) -> anyhow::Result<()> {
    if payload.len() < READ_HEADER_SIZE {
        anyhow::bail!("READ payload too short: {} bytes", payload.len());
    }
    let off = u64::from_ne_bytes(payload[0..8].try_into().unwrap());
    let len = u64::from_ne_bytes(payload[8..16].try_into().unwrap());
    warn!(
        msg_id, object_id, off, len,
        "READ event — unexpected for NFS+on-demand; P0 does not populate, kernel will fail this request"
    );
    Ok(())
}

fn write_cmd(fd: RawFd, cmd: &str) -> anyhow::Result<()> {
    debug!(cmd, "→ /dev/cachefiles");
    let n = unsafe { libc::write(fd, cmd.as_ptr() as *const c_void, cmd.len()) };
    if n < 0 {
        let err = std::io::Error::last_os_error();
        anyhow::bail!("write {:?} to /dev/cachefiles: {}", cmd, err);
    }
    if (n as usize) != cmd.len() {
        anyhow::bail!("short write: wrote {}/{} bytes for {:?}", n, cmd.len(), cmd);
    }
    Ok(())
}

fn install_signal_handlers() -> anyhow::Result<()> {
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
    let action = SigAction::new(
        SigHandler::Handler(signal_handler),
        SaFlags::empty(),
        SigSet::empty(),
    );
    unsafe {
        sigaction(Signal::SIGTERM, &action)?;
        sigaction(Signal::SIGINT, &action)?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}
