//! The daemon's endpoint: binding it privately, the pid file, and on Unix the peer's uid. The
//! endpoint is a socket on Unix and a named pipe on Windows, and `sys` holds what differs.

use crate::layout::{ProjectId, RunDir};
use crate::lock::open_no_follow;
use crate::sys;
use rdy_store::{fsutil, pio};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The longest socket path `bind` accepts, in bytes: `sun_path` is 104 bytes on macOS,
/// one of which is the terminating NUL. A project whose `.rdy/run/rdy.sock` is longer gets the
/// short socket, `/tmp/rdy-<uid>/<hash>.sock` (`RunDir::socket`, D-YMWEM).
pub const MAX_SOCKET_PATH: usize = 103;

/// Refuses a socket path `bind` could not take: over `MAX_SOCKET_PATH` bytes, `bind` fails with
/// `ENAMETOOLONG`. The error names the path, its length and the limit. `run.socket()` is short
/// whenever `.rdy/run/rdy.sock` would be too long, so this is the last guard only: a client
/// checks it before it spawns a daemon that would only die at its bind. On Windows a pipe's name
/// comes from the canonical project path, so a path that cannot be resolved is refused here:
/// `could not resolve the canonical project path {path}: {error}`.
pub fn check_socket_path(run: &RunDir) -> io::Result<()> {
    sys::check_project_root(run.given_root())?;
    let path = run.socket();
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path {} is {} bytes; the limit is {} (sun_path): move or symlink the project to a shorter path",
                path.display(),
                len,
                MAX_SOCKET_PATH
            ),
        ));
    }
    Ok(())
}

/// Binds the project's endpoint (a socket of mode 0o600, or a pipe private to this account) in a
/// private run dir, writes the pid file, and records the endpoint and the project's identity in
/// `daemon.sock` (`RunDir::socket_record`). Returns the listener, the endpoint it bound and the
/// identity of the project's `.rdy` at bind time (`RunDir::project_id`). A failure of the bind
/// itself names the endpoint. No file ever stands at a pipe's name.
///
/// Call only while holding the store lock (§3.1); a client never calls this.
pub async fn bind(run: &RunDir) -> io::Result<(sys::Listener, PathBuf, ProjectId)> {
    run.ensure()?;
    check_socket_path(run)?;
    let project = run.project_id()?;
    let path = run.socket();
    sys::clear_endpoint(&path)?;
    let listener = sys::bind_endpoint(&path)?;
    let mut pid_written = false;
    let done = sys::seal_endpoint(&path)
        .and_then(|()| write_pid_file(run))
        .and_then(|()| {
            pid_written = true;
            let mut record = path.as_os_str().as_encoded_bytes().to_vec();
            record.push(b'\n');
            record.extend_from_slice(format!("{project}\n").as_bytes());
            write_run_file(&run.socket_record(), &record)
        });
    if let Err(e) = done {
        // the endpoint we just bound, and its pid file; no daemon will serve them
        drop(listener);
        let _ = sys::remove_endpoint(&path);
        if pid_written {
            let _ = fs::remove_file(run.pid_file());
        }
        return Err(e);
    }
    Ok((listener, path, project))
}

/// `sys::own_pid_record`, private, written durably (`write_run_file`).
fn write_pid_file(run: &RunDir) -> io::Result<()> {
    write_run_file(&run.pid_file(), &sys::own_pid_record()?)
}

/// Makes `bytes` the durable content of the run-dir file `target`, private to this account: to
/// `<target>.tmp`, never through a link, flushed, then renamed into place and the run dir flushed.
/// A rename that Windows refuses with `PermissionDenied` because another handle holds the target
/// is tried again (`replace_with_retry`, `sys::RUN_FILE_REPLACE_TRIES`).
pub(crate) fn write_run_file(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = target.with_file_name(format!(
        "{}.tmp",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    // never through a symlink: a planted `<name>.tmp` link must not get its target written
    let mut f = open_no_follow(
        sys::private_file(
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true),
        ),
        &tmp,
    )?;
    // a leftover tmp file keeps its old mode through open; set it exactly
    sys::make_private_file(&f)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    replace_with_retry(
        || pio::rename_durable(&tmp, target),
        std::thread::sleep,
        sys::RUN_FILE_REPLACE_TRIES,
    )?;
    fsutil::sync_dir(target.parent().unwrap_or(Path::new(".")))
}

/// Runs `rename` until it succeeds, at most `tries` times, `wait`ing `sys::RUN_FILE_REPLACE_WAIT`
/// between two tries. Only `PermissionDenied` is tried again: another handle may hold the target
/// for a moment (Windows). Any other error, and the last try's error, is returned as it is.
fn replace_with_retry(
    mut rename: impl FnMut() -> io::Result<()>,
    mut wait: impl FnMut(Duration),
    tries: u32,
) -> io::Result<()> {
    let mut left = tries.max(1);
    loop {
        match rename() {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && left > 1 => {
                left -= 1;
                wait(sys::RUN_FILE_REPLACE_WAIT);
            }
            done => return done,
        }
    }
}

/// The effective uid of the process at the other end of a stream, and this process's: a uid is a
/// Unix thing (`sys::Account` is the portable form).
#[cfg(unix)]
pub use crate::sys::{own_uid, peer_uid};

#[cfg(test)]
mod replace_tests {
    use super::{replace_with_retry, write_run_file};
    use crate::sys;
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::io;

    fn denied() -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, "Access is denied")
    }

    #[test]
    fn r59020_run_file_replace_retries_access_denied() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("daemon.sock");
        let tmp = d.path().join("daemon.sock.tmp");
        fs::write(&target, b"old").unwrap();
        fs::write(&tmp, b"new").unwrap();
        let failures = Cell::new(0u32);
        let waits = RefCell::new(Vec::new());
        let r = replace_with_retry(
            || {
                if failures.get() < 3 {
                    failures.set(failures.get() + 1);
                    Err(denied())
                } else {
                    fs::rename(&tmp, &target)
                }
            },
            |w| waits.borrow_mut().push(w),
            20,
        );
        r.unwrap();
        assert_eq!(failures.get(), 3);
        assert_eq!(*waits.borrow(), vec![sys::RUN_FILE_REPLACE_WAIT; 3]);
        assert_eq!(sys::RUN_FILE_REPLACE_WAIT.as_millis(), 50);
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn r59020_run_file_replace_stops_after_its_tries() {
        let tries = Cell::new(0u32);
        let waits = Cell::new(0u32);
        let e = replace_with_retry(
            || {
                tries.set(tries.get() + 1);
                Err(denied())
            },
            |_| waits.set(waits.get() + 1),
            20,
        )
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(e.to_string(), "Access is denied");
        assert_eq!(tries.get(), 20);
        assert_eq!(waits.get(), 19);
    }

    #[test]
    fn r59020_other_errors_do_not_retry() {
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::InvalidInput] {
            let tries = Cell::new(0u32);
            let waits = Cell::new(0u32);
            let e = replace_with_retry(
                || {
                    tries.set(tries.get() + 1);
                    Err(io::Error::new(kind, "no"))
                },
                |_| waits.set(waits.get() + 1),
                20,
            )
            .unwrap_err();
            assert_eq!(e.kind(), kind);
            assert_eq!((tries.get(), waits.get()), (1, 0), "{kind:?}");
        }
        // one try, as on Unix: a denied rename fails at once as well
        let tries = Cell::new(0u32);
        let waits = Cell::new(0u32);
        let e = replace_with_retry(
            || {
                tries.set(tries.get() + 1);
                Err(denied())
            },
            |_| waits.set(waits.get() + 1),
            1,
        )
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!((tries.get(), waits.get()), (1, 0));
    }

    #[test]
    fn r59020_the_platform_limits() {
        assert_eq!(sys::RUN_FILE_REPLACE_WAIT.as_millis(), 50);
        assert_eq!(
            sys::RUN_FILE_REPLACE_TRIES,
            if cfg!(windows) { 20 } else { 1 }
        );
    }

    #[test]
    fn r59020_write_run_file_still_replaces_the_record() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("daemon.pid");
        write_run_file(&target, b"one").unwrap();
        write_run_file(&target, b"two").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::bind;
    use crate::layout::RunDir;
    use crate::sys::path_bytes;
    use rdy_store::fileid::FileId;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn ps7_the_socket_record_is_the_path_bytes_then_the_identity() {
        // under /tmp, so `.rdy/run/rdy.sock` stays short on macOS
        let d = tempfile::Builder::new()
            .prefix("rdy-ps7")
            .tempdir_in("/tmp")
            .unwrap();
        fs::create_dir(d.path().join(".rdy")).unwrap();
        let run = RunDir::of_project(d.path());
        let (listener, socket, project) = bind(&run).await.unwrap();
        let id = FileId::lstat(&d.path().join(".rdy")).unwrap();
        let FileId::Unix { dev, ino } = id else {
            panic!("a Unix identity: {id}")
        };
        let mut want = path_bytes(&socket).to_vec();
        want.push(b'\n');
        want.extend_from_slice(format!("{dev}:{ino}\n").as_bytes());
        assert_eq!(fs::read(run.socket_record()).unwrap(), want);
        assert_eq!(project.to_string(), format!("{dev}:{ino}"));
        assert_eq!(socket, run.socket());
        let mode = fs::symlink_metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the socket's mode");
        drop(listener);
    }
}
