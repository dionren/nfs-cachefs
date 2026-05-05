//! nfs-cachefs library crate.
//!
//! Binaries (`src/main.rs` for the daemon, `src/bin/probe.rs` for the
//! diagnostic) reuse the same protocol, config, cull, and event-loop code
//! by depending on this lib. Keeping it in a lib avoids the previous
//! per-binary copy of validate/write helpers and signal-handler wiring.

pub mod config;
pub mod cull;
pub mod daemon;
pub mod error;
pub mod proto;
pub mod signals;
pub(crate) mod systemd_notify;
