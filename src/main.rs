use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use tracing::{error, info};

use nfs_cachefs::error::Error;
use nfs_cachefs::{config, daemon, proto, signals};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Userspace daemon for Linux fscache + cachefiles (traditional mode)"
)]
struct Args {
    /// Path to the TOML configuration file.
    #[arg(short, long, default_value = "/etc/nfs-cachefs/daemon.toml")]
    config: PathBuf,

    /// Log level (error, warn, info, debug, trace). Takes precedence over
    /// RUST_LOG, which takes precedence over the config file.
    #[arg(long)]
    log_level: Option<String>,
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: i32) {
    STOP.store(true, Ordering::Relaxed);
}

fn main() -> ExitCode {
    let args = Args::parse();
    if let Some(level) = &args.log_level {
        if !config::LOG_LEVELS.contains(&level.as_str()) {
            eprintln!("--log-level must be one of: error, warn, info, debug, trace");
            return ExitCode::from(2);
        }
    }
    let cfg = match config::Config::load(&args.config) {
        Ok(c) => c,
        Err(e) => {
            // Logger isn't up yet; print to stderr.
            eprintln!("config error: {e:#}");
            return ExitCode::from(2);
        }
    };

    init_tracing(args.log_level.as_deref(), &cfg.log.level, &cfg.log.format);

    if let Err(e) = signals::install(handle_signal) {
        error!(error = %e, "failed to install signal handlers");
        return ExitCode::FAILURE;
    }

    info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %args.config.display(),
        "nfs-cachefs daemon starting"
    );

    let dev = match proto::Device::open() {
        Ok(d) => d,
        Err(e) => {
            error!(error = %e, "failed to open /dev/cachefiles (need CAP_SYS_ADMIN; another daemon holding it?)");
            return ExitCode::FAILURE;
        }
    };

    let d = daemon::Daemon {
        dev,
        config: &cfg,
        stop: &STOP,
    };
    match d.run() {
        Ok(()) => {
            info!("daemon exited cleanly");
            ExitCode::SUCCESS
        }
        Err(Error::Interrupted) => {
            info!("stopped before the cache was bound");
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!(error = %e, "daemon exited with error");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing(cli_level: Option<&str>, config_level: &str, format: &str) {
    use std::io::IsTerminal;
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = match cli_level {
        Some(level) => EnvFilter::new(level),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(config_level)),
    };
    // Color only on a terminal. Under systemd stdout goes to the journal,
    // where escape codes are stored in the message and break grep.
    let builder = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(std::io::stdout().is_terminal());
    // The journal timestamps every line itself (systemd sets JOURNAL_STREAM
    // when stdout goes there); a second, UTC timestamp only confuses.
    let journal = std::env::var_os("JOURNAL_STREAM").is_some();
    match (format, journal) {
        ("json", _) => builder.json().init(),
        (_, true) => builder.compact().without_time().init(),
        (_, false) => builder.compact().init(),
    }
}
