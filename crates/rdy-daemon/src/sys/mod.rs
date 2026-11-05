//! The daemon's platform calls, one seam for every Unix-only call the daemon makes (FW-XP1):
//! - transport: the local socket's listener, stream and halves, and paths as bytes;
//! - peer identity: the uid and pid at the other end of a stream, and this process's uid;
//! - no-follow opens: a run-dir file that is a symbolic link is refused;
//! - private files and directories: modes 0o600 and 0o700, the owner, and hard-link support;
//!
//! The no-follow open, this process's uid, a file's owner and the private file and directory
//! calls (`open_no_follow`, `own_uid`, `owner`, `private_file`, `make_private_file`,
//! `create_private_dir`, `make_private_dir`, `is_private_dir`) live in the store's seam,
//! `rdy_store::pio`, so engine code can use them too; this seam re-exports them under the same
//! names. `make_private_path` stays here.
//! - process lifecycle: a new session on spawn, exit codes, liveness, `SIGTERM` and `setsid`;
//! - what a stop's wait knows of an old daemon ([`Liveness`], `process_liveness`,
//!   `endpoint_liveness`). The contract for every platform: only a proved end is `Gone`. Access
//!   denied and an answering endpoint are `Live`, anything else unclear is `Ambiguous`, and a
//!   caller waits on both. A named pipe is no filesystem object, so its probe connects, never
//!   looks for a name;
//! - stop signals: `SIGINT` and `SIGTERM` in a tokio runtime;
//! - the startup environment: the diagram crate's variables, removed before anything else runs;
//! - the site socket's option before its bind (D-FPKPS, `configure_site_socket`): `SO_REUSEADDR`
//!   on Unix, `SO_EXCLUSIVEADDRUSE` on Windows (built and tested on its own by the isolated probe
//!   in `probes/site-socket`).

mod site_socket;
#[cfg(unix)]
mod unix;
pub(crate) use site_socket::configure_site_socket;
#[cfg(unix)]
pub use unix::*;

#[cfg(not(unix))]
compile_error!("rdy_daemon::sys has no implementation for this platform yet (FW-XP1 Phase 4)");

/// What is known of an old daemon's process or endpoint while a stop waits for it to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// It has ended: proved, not assumed.
    Gone,
    /// It is there: it answered, or the system refused this user access to it.
    Live,
    /// The system's answer proves neither. A wait counts this as still live.
    Ambiguous,
}

/// The variables the pinned diagram crate reads at run time. Each one changes its drawing.
const MMDFLUX_ENVIRONMENT: [&str; 6] = [
    "MMDFLUX_DEBUG_DISABLE_LR_CRISS_CROSS_OVERLAP",
    "MMDFLUX_DEBUG_DISABLE_LR_VERTICAL_OVERLAP",
    "MMDFLUX_SKIP_TITLE_NODES",
    "MMDFLUX_DEBUG_BORDER_NODES",
    "MMDFLUX_DEBUG_PIPELINE",
    "MMDFLUX_DEBUG_LAYOUT",
];

/// Removes the pinned diagram crate's variables from this process's environment, so every
/// drawing uses its default routing whatever the caller set (D-E8P8J). The daemon, the MCP
/// server and the render process are this binary, and inherit the clean environment.
///
/// Call it once, as the first statement of the binary's `main`, while the process has one
/// thread. Never call it from a renderer, a library host or a test harness.
pub fn scrub_mmdflux_environment_at_startup() {
    for name in MMDFLUX_ENVIRONMENT {
        #[allow(unsafe_code)]
        // SAFETY: this runs first in `main`, before argument parsing, logging, a runtime or any
        // thread exists, so no other thread can read or write the environment at the same time.
        unsafe {
            std::env::remove_var(name)
        };
    }
}
