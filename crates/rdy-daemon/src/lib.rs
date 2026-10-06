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
    E_BAD_INPUT, E_DAEMON_GONE, E_DAEMON_START, E_DAEMON_VERSION, E_LIMIT, E_NO_SESSION, ErrorReply,
};
pub use handler::Handler;
pub use hello::{HELLO_TIMEOUT, Hello, HelloReply, MAX_HELLO_BYTES, PROTO_VERSION, read_hello};
pub use layout::{LOG_FILE, PID_FILE, RUN_DIR, RunDir, SOCKET_FILE, START_LOCK_FILE};
pub use session::{Proto, SessionId, SessionInfo, Sessions};
pub use socket::MAX_SOCKET_PATH;
pub mod client;
#[doc(hidden)]
pub mod lock;
pub mod spawn;

pub use client::{
    Attempt, Client, DEFAULT_START_TIMEOUT, HELD_GRACE, LOCK_RETRY, LOG_TAIL_LINES, Pending,
    SpawnFn, StartOptions, connect, connect_backoff, connect_or_start, log_tail, log_tail_since,
};
pub use spawn::{Spawned, detach, process_alive, serve_command, spawn_serve};
pub mod shared;
pub mod writer;

pub use shared::{DaemonEngine, Shared};
pub use writer::{CHECKPOINT_TICK, Writer, WriterHandle};
pub mod ops;

pub use ops::{OPS, OpContext, dispatch, quarantine};
pub mod serve;

pub use serve::{CtlSession, ServeError, ServeHandler, ServeOptions, ServeRun, serve, start};
pub mod proxy;

pub use proxy::run_stdio;

pub mod watch;

pub use watch::PackWatch;
