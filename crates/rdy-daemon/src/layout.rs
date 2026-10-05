//! Where the daemon's files live: `<project>/.rdy/run/`.

use std::path::{Path, PathBuf};

pub const RUN_DIR: &str = "run";
pub const SOCKET_FILE: &str = "rdy.sock";
pub const START_LOCK_FILE: &str = "start.lock";
pub const PID_FILE: &str = "daemon.pid";
pub const LOG_FILE: &str = "daemon.log";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDir(PathBuf);

impl RunDir {
    pub fn of_project(root: &Path) -> RunDir {
        RunDir(root.join(".rdy").join(RUN_DIR))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn socket(&self) -> PathBuf {
        self.0.join(SOCKET_FILE)
    }

    pub fn start_lock(&self) -> PathBuf {
        self.0.join(START_LOCK_FILE)
    }

    pub fn pid_file(&self) -> PathBuf {
        self.0.join(PID_FILE)
    }

    pub fn log_file(&self) -> PathBuf {
        self.0.join(LOG_FILE)
    }

    /// Creates the directory (mode 0o700) if needed. Task 2 implements this.
    pub fn ensure(&self) -> std::io::Result<()> {
        todo!("implemented by the socket task")
    }
}
