//! The run dir's file lock and no-follow opens. The lock itself lives below the engine, with the
//! project's start lock (`rdy_store::start_lock`).

pub use rdy_store::start_lock::FileLock;

/// Opens `path` with `opts` and `O_NOFOLLOW` (`sys::open_no_follow`): a run-dir file that is a
/// symlink is refused, never followed.
pub(crate) use crate::sys::open_no_follow;
