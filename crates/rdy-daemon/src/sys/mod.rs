//! The daemon's platform calls, one seam for every Unix-only call the daemon makes (FW-XP1):
//! - transport: the local socket's listener, stream and halves, and paths as bytes;
//! - peer identity: the uid and pid at the other end of a stream, and this process's uid;
//! - no-follow opens: a run-dir file that is a symbolic link is refused;
//! - private files and directories: modes 0o600 and 0o700, the owner, and hard-link support;
//! - process lifecycle: a new session on spawn, exit codes, liveness, `SIGTERM` and `setsid`;
//! - stop signals: `SIGINT` and `SIGTERM` in a tokio runtime.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(not(unix))]
compile_error!("rdy_daemon::sys has no implementation for this platform yet (FW-XP1 Phase 4)");
