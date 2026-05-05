use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use tracing::{error, info};

use nfs_cachefs::{config, cull, daemon, proto, signals};

#[derive(Parser, Debug)]
#[command(version, about = "Userspace daemon for Linux fscache + cachefiles (traditional mode)")]
struct Args {
    /// Path to the TOML configuration file.
    #[arg(short, long, default_value = "/etc/nfs-cachefs/daemon.toml")]
    config: PathBuf,

    /// Override log level (else read from config / RUST_LOG).
    #[arg(long)]
    log_level: Option<String>,
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: i32) {
    STOP.store(true, Ordering::Relaxed);
}

fn main() -> ExitCode {
    let args = Args::parse();
    let cfg = match config::Config::load(&args.config) {
        Ok(c) => c,
        Err(e) => {
            // Logger isn't up yet; print to stderr.
            eprintln!("config error: {e:#}");
            return ExitCode::from(2);
        }
    };

    init_tracing(args.log_level.as_deref().unwrap_or(&cfg.log.level), &cfg.log.format);

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

    let cmd = cfg.as_config_cmd();
    let cull_ctx = cull::CullCtx {
        cache_root: cfg.cache_dir.clone(),
        batch_size: cfg.cull.batch_size,
    };

    let d = daemon::Daemon {
        dev,
        config: cmd,
        cull: cull_ctx,
        stop: &STOP,
    };

    if let Err(e) = d.run() {
        error!(error = %e, "daemon exited with error");
        return ExitCode::FAILURE;
    }

    info!("daemon exited cleanly");
    ExitCode::SUCCESS
}

fn init_tracing(level: &str, format: &str) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));
    let builder = fmt().with_env_filter(filter).with_target(false);
    match format {
        "json" => {
            builder.json().init();
        }
        _ => {
            builder.compact().init();
        }
    }
}
