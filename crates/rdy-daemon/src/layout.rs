//! Where the daemon's files live: `<project>/.rdy/run/`. The socket is `.rdy/run/rdy.sock` when
//! that path fits `sun_path`, else `/tmp/rdy-<uid>/<hash>.sock` in a directory private to the
//! user (D-YMWEM).

use crate::lock::open_no_follow;
use crate::socket::{MAX_SOCKET_PATH, own_uid};
use rdy_model::Hash;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const RUN_DIR: &str = "run";
pub const SOCKET_FILE: &str = "rdy.sock";
pub const START_LOCK_FILE: &str = "start.lock";
pub const PID_FILE: &str = "daemon.pid";
pub const LOG_FILE: &str = "daemon.log";
/// The run dir's own `.gitignore`: its files are the daemon's, never a checkout's.
const GITIGNORE_FILE: &str = ".gitignore";
/// Where the short socket's directory, `rdy-<uid>`, lives (D-YMWEM).
pub const SHORT_SOCKET_ROOT: &str = "/tmp";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDir(PathBuf);

impl RunDir {
    pub fn of_project(root: &Path) -> RunDir {
        RunDir(root.join(".rdy").join(RUN_DIR))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// This run dir made absolute against the working directory (`std::path::absolute`), so the
    /// daemon spawned in the project names the same files.
    pub fn absolute(&self) -> io::Result<RunDir> {
        std::path::absolute(&self.0).map(RunDir)
    }

    /// The project's socket (D-YMWEM). The project root is canonical (`fs::canonicalize`, else
    /// `std::path::absolute`), so every spelling of one project gives one socket. It is
    /// `<root>/.rdy/run/rdy.sock` when that is at most `MAX_SOCKET_PATH` bytes, else
    /// `/tmp/rdy-<uid>/<hash of the root>.sock`.
    pub fn socket(&self) -> PathBuf {
        self.socket_place().0
    }

    /// The socket when it is outside `.rdy/run/` (`/tmp/rdy-<uid>/<hash>.sock`), else `None`.
    pub fn short_socket(&self) -> Option<PathBuf> {
        match self.socket_place() {
            (path, true) => Some(path),
            (_, false) => None,
        }
    }

    /// The socket, and whether it is the short one.
    fn socket_place(&self) -> (PathBuf, bool) {
        let root = self.canonical_root();
        let long = root.join(".rdy").join(RUN_DIR).join(SOCKET_FILE);
        if long.as_os_str().len() <= MAX_SOCKET_PATH {
            return (long, false);
        }
        let hash = Hash::of(root.as_os_str().as_bytes()).short();
        (short_dir(own_uid()).join(format!("{hash}.sock")), true)
    }

    /// The project root (the run dir's grandparent), canonical: `fs::canonicalize`, else
    /// `std::path::absolute` (a root that does not exist), else as given.
    fn canonical_root(&self) -> PathBuf {
        let root = match self.0.parent().and_then(Path::parent) {
            Some(r) if !r.as_os_str().is_empty() => r,
            _ => Path::new("."),
        };
        fs::canonicalize(root)
            .or_else(|_| std::path::absolute(root))
            .unwrap_or_else(|_| root.to_path_buf())
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
    /// When the socket is the short one, makes sure its directory is private to this user
    /// (`ensure_short_dir`); a refusal is a [`NotPrivateDir`] error.
    pub fn ensure(&self) -> io::Result<()> {
        self.ensure_dir()?;
        self.ignore_self()?;
        match self.short_socket().as_deref().and_then(Path::parent) {
            Some(dir) => ensure_short_dir(dir),
            None => Ok(()),
        }
    }

    fn ignore_self(&self) -> io::Result<()> {
        self.ignore_self_with(|f| f.write_all(b"*\n"), |a, b| fs::hard_link(a, b))
    }

    /// Writes the `.gitignore` atomically and without clobbering: the content goes to a unique
    /// temp file, which is synced and then hard-linked to the name (`link`). On a file system
    /// without hard links there is no `.gitignore`: it is best effort. A failed write or a crash
    /// never leaves a partial `.gitignore` that later calls would take as present. `link` fails
    /// with `EEXIST` when anything (a link included) stands at the name, and never follows it.
    /// The temp file is removed on every path.
    fn ignore_self_with(
        &self,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        link: impl FnOnce(&Path, &Path) -> io::Result<()>,
    ) -> io::Result<()> {
        let path = self.0.join(GITIGNORE_FILE);
        if fs::symlink_metadata(&path).is_ok() {
            return Ok(());
        }
        let (temp, mut f) = self.create_temp()?;
        let _remove = RemoveOnDrop(&temp);
        write(&mut f)?;
        f.sync_all()?;
        drop(f);
        match link(&temp, &path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            // a file system without hard links: the .gitignore is best effort, so there is none,
            // never a non-atomic write
            Err(e) if no_hard_links(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// A new temp file in the run dir (`create_new`, O_NOFOLLOW, mode 0o600) under a name no
    /// other caller uses.
    fn create_temp(&self) -> io::Result<(PathBuf, File)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut attempts = 0;
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let temp = self
                .0
                .join(format!("{GITIGNORE_FILE}.tmp-{}-{n}", std::process::id()));
            match open_no_follow(
                OpenOptions::new().write(true).create_new(true).mode(0o600),
                &temp,
            ) {
                Ok(f) => return Ok((temp, f)),
                // left by a crashed process with the same pid: take the next name
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempts < 64 => {
                    attempts += 1;
                }
                Err(e) => return Err(e),
            }
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

/// `/tmp/rdy-<uid>`: the directory of the short sockets of user `uid`.
fn short_dir(uid: u32) -> PathBuf {
    Path::new(SHORT_SOCKET_ROOT).join(format!("rdy-{uid}"))
}

/// A short socket directory that rdy refuses to use: not a real directory (a symlink, a file),
/// or not owned by this user.
#[derive(Debug)]
pub struct NotPrivateDir(pub PathBuf);

impl fmt::Display for NotPrivateDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is not a private directory of this user, so rdy cannot put the socket there",
            self.0.display()
        )
    }
}

impl std::error::Error for NotPrivateDir {}

/// `ensure_short_dir_as` for this process's user.
fn ensure_short_dir(dir: &Path) -> io::Result<()> {
    ensure_short_dir_as(dir, own_uid())
}

/// Makes `dir` a directory private to user `uid` (D-YMWEM): creates it with mode 0o700 when it
/// is missing; tightens a directory of `uid` with another mode to 0o700; refuses anything else
/// (a symlink, a file, a directory of another user) with [`NotPrivateDir`], and leaves it as it
/// is. It never follows a symlink.
fn ensure_short_dir_as(dir: &Path, uid: u32) -> io::Result<()> {
    let m = match fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                // another process made it first: check what it made
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            fs::symlink_metadata(dir)?
        }
        Err(e) => return Err(e),
    };
    if !m.is_dir() || m.uid() != uid {
        return Err(io::Error::other(NotPrivateDir(dir.to_path_buf())));
    }
    // the umask may have narrowed a new directory's mode, and an old one may be open to others
    if m.mode() & 0o777 != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Removes a temp file when dropped, so every exit path cleans it up.
struct RemoveOnDrop<'a>(&'a Path);

impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

fn not_a_dir(path: &Path) -> io::Error {
    io::Error::other(format!("{} exists and is not a directory", path.display()))
}

/// Whether `e` says the file system has no hard links (`EPERM`, `ENOTSUP`, `EOPNOTSUPP`).
fn no_hard_links(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(n) if n == libc::EPERM || n == libc::ENOTSUP || n == libc::EOPNOTSUPP
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn msw15_a_failed_gitignore_write_leaves_no_gitignore_and_no_temp_file() {
        let d = tempfile::tempdir().unwrap();
        let run = RunDir::of_project(d.path());
        run.ensure_dir().unwrap();
        let err = run
            .ignore_self_with(
                |_| Err(io::Error::from_raw_os_error(libc::ENOSPC)),
                |a, b| fs::hard_link(a, b),
            )
            .unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOSPC), "{err}");
        assert!(
            fs::symlink_metadata(run.path().join(GITIGNORE_FILE)).is_err(),
            "a failed write left a .gitignore behind"
        );
        assert_eq!(
            names_in(run.path()),
            Vec::<String>::new(),
            "a temp file was left behind"
        );
        // the next ensure repairs it
        run.ensure().unwrap();
        assert_eq!(
            fs::read_to_string(run.path().join(GITIGNORE_FILE)).unwrap(),
            "*\n"
        );
        assert_eq!(names_in(run.path()), vec![GITIGNORE_FILE.to_string()]);
    }

    /// Final review M9: on a file system without hard links, the .gitignore is best effort:
    /// ensure succeeds with no .gitignore and no temp file, never a non-atomic write.
    #[test]
    fn msw15_a_file_system_without_hard_links_has_no_gitignore_and_still_ensures() {
        let d = tempfile::tempdir().unwrap();
        let run = RunDir::of_project(d.path());
        run.ensure_dir().unwrap();
        for errno in [libc::EPERM, libc::ENOTSUP, libc::EOPNOTSUPP] {
            run.ignore_self_with(
                |f| f.write_all(b"*\n"),
                |_, _| Err(io::Error::from_raw_os_error(errno)),
            )
            .unwrap_or_else(|e| panic!("errno {errno}: {e}"));
            assert_eq!(names_in(run.path()), Vec::<String>::new(), "errno {errno}");
        }
        // any other link error still fails
        let err = run
            .ignore_self_with(
                |f| f.write_all(b"*\n"),
                |_, _| Err(io::Error::from_raw_os_error(libc::EIO)),
            )
            .unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EIO), "{err}");
        assert_eq!(names_in(run.path()), Vec::<String>::new());
    }

    #[test]
    fn msw15_an_existing_gitignore_is_left_untouched_and_no_temp_file_remains() {
        let d = tempfile::tempdir().unwrap();
        let run = RunDir::of_project(d.path());
        run.ensure_dir().unwrap();
        let path = run.path().join(GITIGNORE_FILE);
        fs::write(&path, b"").unwrap();
        run.ensure().unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            b"",
            "an empty .gitignore was rewritten"
        );
        fs::write(&path, b"daemon.log\n").unwrap();
        run.ensure().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "daemon.log\n");
        assert_eq!(names_in(run.path()), vec![GITIGNORE_FILE.to_string()]);
    }

    #[test]
    fn msw15_a_link_at_the_gitignore_name_is_not_written_through() {
        let d = tempfile::tempdir().unwrap();
        let run = RunDir::of_project(d.path());
        run.ensure_dir().unwrap();
        let target = d.path().join("elsewhere");
        let path = run.path().join(GITIGNORE_FILE);
        std::os::unix::fs::symlink(&target, &path).unwrap();
        run.ensure().unwrap();
        assert!(!target.exists(), "the link was followed");
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(names_in(run.path()), vec![GITIGNORE_FILE.to_string()]);
    }

    #[test]
    fn df3_a_short_socket_directory_that_is_not_private_is_refused() {
        use std::os::unix::fs::MetadataExt;
        let me = crate::socket::own_uid();
        let d = tempfile::tempdir().unwrap();
        let refused = |path: &Path, e: io::Error| {
            assert_eq!(
                e.to_string(),
                format!(
                    "{} is not a private directory of this user, so rdy cannot put the socket there",
                    path.display()
                )
            );
        };
        // (a) a symlink to a private directory of this user
        let real = d.path().join("real");
        fs::DirBuilder::new().mode(0o700).create(&real).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        refused(&link, ensure_short_dir(&link).unwrap_err());
        // (b) a regular file
        let file = d.path().join("file");
        fs::write(&file, b"").unwrap();
        refused(&file, ensure_short_dir(&file).unwrap_err());
        // (c) a directory of this user open to others is tightened
        let open = d.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_short_dir(&open).unwrap();
        let m = fs::symlink_metadata(&open).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o700);
        assert_eq!(m.uid(), me);
        // (d) a directory of another user is refused and left as it is
        fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
        refused(&open, ensure_short_dir_as(&open, me + 1).unwrap_err());
        assert_eq!(
            fs::symlink_metadata(&open).unwrap().permissions().mode() & 0o777,
            0o755
        );
        // (e) a missing directory is created with mode 0700
        let missing = d.path().join("missing");
        ensure_short_dir(&missing).unwrap();
        let m = fs::symlink_metadata(&missing).unwrap();
        assert!(m.is_dir());
        assert_eq!(m.permissions().mode() & 0o777, 0o700);
        // the short directory's name
        assert_eq!(short_dir(501), PathBuf::from("/tmp/rdy-501"));
    }
}
