//! Shared signal-handler installation.
//!
//! The handler itself and the `AtomicBool` it stores into live in each
//! binary — `extern "C" fn(i32)` cannot capture environment, so the lib
//! cannot own them. This module just wires SIGTERM/SIGINT/SIGHUP to a
//! caller-supplied handler.
//!
//! SIGHUP is included because the default action is to terminate the
//! process: a stray `kill -HUP` or a closed controlling tty would
//! otherwise cause an abrupt exit (kernel auto-unbinds, but the operator
//! sees a daemon that "just died"). Treating it as graceful shutdown
//! mirrors how SIGTERM is handled.
//!
//! The handler must be async-signal-safe: storing into an AtomicBool is.
//! Anything else (logging, allocation) is not.
//!
//! ```no_run
//! use std::sync::atomic::{AtomicBool, Ordering};
//! static STOP: AtomicBool = AtomicBool::new(false);
//! extern "C" fn on_sig(_: i32) { STOP.store(true, Ordering::Relaxed); }
//! nfs_cachefs::signals::install(on_sig).unwrap();
//! ```

use crate::error::{Error, Result};

/// Install `handler` for SIGTERM, SIGINT, and SIGHUP.
pub fn install(handler: extern "C" fn(i32)) -> Result<()> {
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
    let action = SigAction::new(
        SigHandler::Handler(handler),
        SaFlags::empty(),
        SigSet::empty(),
    );
    for sig in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
        // SAFETY: action holds a plain function pointer with no captured
        // state; sigaction installs a handler whose signature matches
        // SigHandler::Handler. The handler itself must be
        // async-signal-safe (caller's responsibility).
        unsafe {
            sigaction(sig, &action).map_err(|e| {
                Error::Io(std::io::Error::other(format!("sigaction({sig:?}): {e}")))
            })?;
        }
    }
    Ok(())
}
