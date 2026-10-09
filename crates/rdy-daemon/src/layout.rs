//! Where the daemon's files live: `<project>/.rdy/run/`.

use crate::lock::open_no_follow;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const RUN_DIR: &str = "run";
pub const SOCKET_FILE: &str = "rdy.sock";
pub const START_LOCK_FILE: &str = "start.lock";
pub const PID_FILE: &str = "daemon.pid";
pub const LOG_FILE: &str = "daemon.log";
/// The run dir's own `.gitignore`: its files are the daemon's, never a checkout's.
const GITIGNORE_FILE: &str = ".gitignore";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDir(PathBuf);

impl RunDir {
    pub fn of_project(root: &Path) -> RunDir {
        RunDir(root.join(".rdy").join(RUN_DIR))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// This run dir made absolute against the working directory (`std::path::absolute`), so a
    /// check of its length sees the path the daemon binds.
    pub fn absolute(&self) -> io::Result<RunDir> {
        std::path::absolute(&self.0).map(RunDir)
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
    /// `run` directory to 0o700; refuses anything else (a file, a symlink) in its place. Then
    /// writes the run dir's `.gitignore` (`*`) when it is missing, so git ignores the socket, pid,
    /// lock and log; whatever already stands at that name, a link included, is left untouched.
    pub fn ensure(&self) -> io::Result<()> {
        self.ensure_dir()?;
        self.ignore_self()
    }

    fn ignore_self(&self) -> io::Result<()> {
        let path = self.0.join(GITIGNORE_FILE);
        // `create_new` (O_EXCL) never follows a link at the name, and O_NOFOLLOW refuses one
        match open_no_follow(
            OpenOptions::new().write(true).create_new(true).mode(0o600),
            &path,
        ) {
            Ok(mut f) => f.write_all(b"*\n"),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn ensure_dir(&self) -> io::Result<()> {
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
