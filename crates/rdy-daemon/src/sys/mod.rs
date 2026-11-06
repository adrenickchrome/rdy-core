//! The daemon's platform calls, one seam for every Unix-only call the daemon makes (FW-XP1):
//! - transport: the local endpoint's listener and stream, and paths as bytes. A named pipe on
//!   Windows and a socket elsewhere; `Listener::accept` gives a connected `Stream`, and `split`
//!   gives its halves, the same types on every platform;
//! - peer identity: this process's [`Account`] (a uid on Unix, a full SID on Windows), and each
//!   side's check of the account at the other end of a stream before any hello byte moves
//!   (`check_client`, `check_daemon`), which also give that peer's process (`PeerProcess`);
//! - a session's peer account (D-FARN9, `SessionPeer`): each platform shows only its own field;
//! - no-follow opens: a run-dir file that is a symbolic link is refused;
//! - private files and directories: modes 0o600 and 0o700, the owner, and hard-link support;
//!
//! The no-follow open, this process's uid, a file's owner and the private file and directory
//! calls (`open_no_follow`, `own_uid`, `owner`, `private_file`, `make_private_file`,
//! `create_private_dir`, `make_private_dir`, `is_private_dir`) live in the store's seam,
//! `rdy_store::pio`, so engine code can use them too; this seam re-exports them under the same
//! names. `make_private_path` stays here;
//! - the project's endpoint and run state: which endpoint a canonical project root has
//!   (`project_endpoint`), its lifecycle (`clear_endpoint`, `bind_endpoint`, `seal_endpoint`,
//!   `remove_endpoint`; a pipe is no file, so these do nothing to the file system on Windows), what
//!   may be asked of a name without connecting to it (`endpoint_is_link`, `endpoint_identity`),
//!   how a bind can fail (`BIND_REFUSED`, `name_is_held`), the
//!   exact grammars of `daemon.sock` and `daemon.pid` (`parse_endpoint_record`, `parse_pid_record`,
//!   and [`records`] for the Windows forms), what stands at a run directory's path (`dir_state`)
//!   and whether a run file may be believed (`run_file_is_private`);
//! - process lifecycle: a new session on spawn, where a started daemon runs (`daemon_start_dir`),
//!   exit codes, liveness and leaving the caller's session (`detach`). `SIGTERM` by pid is Unix
//!   only: Windows never ends a process by its pid;
//! - what the project check watches beside `.rdy` (`project_parts`): nothing on Unix, and the store
//!   on Windows;
//! - what a stop's wait knows of an old daemon ([`Liveness`], `process_liveness`,
//!   `endpoint_liveness`). The contract for every platform: only a proved end is `Gone`. Access
//!   denied and an answering endpoint are `Live`, anything else unclear is `Ambiguous`, and a
//!   caller waits on both. A named pipe is no filesystem object, so its probe connects, never
//!   looks for a name;
//! - stop signals, installed for one project's canonical root: `SIGINT` and `SIGTERM` in a tokio
//!   runtime on Unix, and the project's private named event, Ctrl-C and Ctrl-Break on Windows. A
//!   daemon registers what holds no name before its store opens (`early`) and takes the project's
//!   name once it has the store (`claim_project`);
//! - the stop of a daemon of another version (`stop_refused_daemon`, under the proofs of
//!   [`stop_proof`]): a signal on Unix, and on Windows the event, then after a grace period a
//!   forced end through a handle held since the peer check;
//! - the caller's SSH agent: `AgentStream`, `AgentCancel`, `agent_connect`, `agent_handle` and
//!   `agent_close`. A socket on Unix, and on Windows only the local named pipe the caller names.
//!   Nothing else is looked for, and a cancel or close ends a call that waits;
//! - the caller's interrupt while it signs (`catch_interrupt`): `SIGINT` on Unix, and Ctrl-C and
//!   Ctrl-Break on Windows;
//! - the startup environment: the diagram crate's variables, removed before anything else runs;
//! - the site socket's option before its bind (D-FPKPS, `configure_site_socket`): `SO_REUSEADDR`
//!   on Unix, `SO_EXCLUSIVEADDRUSE` on Windows (built and tested on its own by the isolated probe
//!   in `probes/site-socket`).

pub mod records;
mod site_socket;
pub mod stop_proof;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
pub(crate) use site_socket::configure_site_socket;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(any(unix, windows)))]
compile_error!("rdy_daemon::sys has no implementation for this platform (FW-XP1 D-1RXEA)");

/// The bytes of the canonical path `p` that a port hash takes (the site's ports, D-YMWEM and
/// D-DK03K). On Unix they are the path's own bytes, so the ports are as they were. On Windows
/// they are the path's UTF-16LE code units, as the pipe name's hash takes them, so no native
/// separator is converted and no unpaired surrogate is lost.
#[cfg(unix)]
pub fn path_hash_bytes(p: &std::path::Path) -> std::borrow::Cow<'_, [u8]> {
    std::borrow::Cow::Borrowed(path_bytes(p))
}

/// See the Unix form: the path's UTF-16LE code units.
#[cfg(windows)]
pub fn path_hash_bytes(p: &std::path::Path) -> std::borrow::Cow<'_, [u8]> {
    use std::os::windows::ffi::OsStrExt;
    std::borrow::Cow::Owned(
        p.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect(),
    )
}

/// The read half of a `Stream`.
pub type ReadHalf = tokio::io::ReadHalf<Stream>;
/// The write half of a `Stream`.
pub type WriteHalf = tokio::io::WriteHalf<Stream>;

/// Splits `stream` into halves that can move to different tasks. Shutting down the write half
/// ends the peer's reads; the connection itself closes when both halves are gone.
pub fn split(stream: Stream) -> (ReadHalf, WriteHalf) {
    tokio::io::split(stream)
}

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

/// What stands at a run directory's path (`dir_state`), never followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirState {
    /// A directory that is private to this account.
    Private,
    /// A directory that is not: `make_private_dir` narrows it, or refuses one of another account.
    NotPrivate,
    /// Something else.
    NotDir,
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
