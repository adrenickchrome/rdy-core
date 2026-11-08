//! The daemon's endpoint: binding it privately, the pid file, and on Unix the peer's uid. The
//! endpoint is a socket on Unix and a named pipe on Windows, and `sys` holds what differs.

use crate::layout::{ProjectId, RunDir};
use crate::lock::open_no_follow;
use crate::sys;
use rdy_store::{fsutil, pio};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
    pio::rename_durable(&tmp, target)?;
    fsutil::sync_dir(target.parent().unwrap_or(Path::new(".")))
}

/// The effective uid of the process at the other end of a stream, and this process's: a uid is a
/// Unix thing (`sys::Account` is the portable form).
#[cfg(unix)]
pub use crate::sys::{own_uid, peer_uid};

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
