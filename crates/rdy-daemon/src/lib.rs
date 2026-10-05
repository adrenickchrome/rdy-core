//! The rdy daemon transport: a per-project Unix socket serving `ctl` (line-delimited JSON
//! requests) and `mcp` connections, with a session table and a handler trait the host fills in.

pub mod ctl;
pub mod daemon;
pub mod error;
pub mod handler;
pub mod hello;
pub mod layout;
pub mod session;
pub mod socket;

pub use ctl::{CtlReply, CtlRequest, DaemonInfo, MAX_CTL_LINE_BYTES, serve_ctl};
pub use daemon::{
    DEFAULT_IDLE_TIMEOUT, Daemon, DaemonConfig, DaemonHandle, ExitReason, PROGRESS_MIN_INTERVAL,
    Readiness, STOP_GRACE,
};
pub use error::{
    E_BAD_INPUT, E_DAEMON_GONE, E_DAEMON_START, E_DAEMON_VERSION, E_LIMIT, ErrorReply,
};
pub use handler::Handler;
pub use hello::{HELLO_TIMEOUT, Hello, HelloReply, MAX_HELLO_BYTES, PROTO_VERSION, read_hello};
pub use layout::{LOG_FILE, PID_FILE, RUN_DIR, RunDir, SOCKET_FILE, START_LOCK_FILE};
pub use session::{Proto, SessionId, SessionInfo, Sessions};
pub use socket::MAX_SOCKET_PATH;
