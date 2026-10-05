//! Where the daemon's files live: `<project>/.rdy/run/`.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
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

    /// Creates `.rdy/run` (and `.rdy` if missing) with mode 0o700 on `run`; tightens an existing
    /// `run` directory to 0o700; refuses anything else (a file, a symlink) in its place.
    pub fn ensure(&self) -> io::Result<()> {
        let path = &self.0;
        match fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() => {
                if m.permissions().mode() & 0o777 != 0o700 {
                    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
                }
                Ok(())
            }
            Ok(_) => Err(not_a_dir(path)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                match fs::DirBuilder::new().mode(0o700).create(path) {
                    Ok(()) => {}
                    // another process made it first: check what it made
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        if !fs::symlink_metadata(path)?.is_dir() {
                            return Err(not_a_dir(path));
                        }
                    }
                    Err(e) => return Err(e),
                }
                // the umask may have narrowed the mode; set it exactly
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            }
            Err(e) => Err(e),
        }
    }
}

fn not_a_dir(path: &Path) -> io::Error {
    io::Error::other(format!("{} exists and is not a directory", path.display()))
}
