//! The daemon's socket: binding it privately, the pid file, and the peer's uid.

use crate::layout::RunDir;
use crate::lock::open_no_follow;
use std::fs;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};

/// The longest socket path `bind` accepts, in bytes: `sun_path` is 104 bytes on macOS,
/// one of which is the terminating NUL. A project whose `.rdy/run/rdy.sock` is longer gets the
/// short socket, `/tmp/rdy-<uid>/<hash>.sock` (`RunDir::socket`, D-YMWEM).
pub const MAX_SOCKET_PATH: usize = 103;

/// Refuses a socket path `bind` could not take: over `MAX_SOCKET_PATH` bytes, `bind` fails with
/// `ENAMETOOLONG`. The error names the path, its length and the limit. `run.socket()` is short
/// whenever `.rdy/run/rdy.sock` would be too long, so this is the last guard only: a client
/// checks it before it spawns a daemon that would only die at its bind.
pub fn check_socket_path(run: &RunDir) -> io::Result<()> {
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

/// Binds the project's socket (mode 0o600) in a private run dir, writes the pid file, and
/// records the socket's path in `daemon.sock` (`RunDir::socket_record`). Returns the listener
/// and the path it bound. A failure of the bind itself names the socket's path.
///
/// Call only while holding the store lock (§3.1); a client never calls this.
pub async fn bind(run: &RunDir) -> io::Result<(UnixListener, PathBuf)> {
    run.ensure()?;
    check_socket_path(run)?;
    let path = run.socket();
    let named = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", path.display()));
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(named(e)),
    }
    let listener = UnixListener::bind(&path).map_err(named)?;
    let mut pid_written = false;
    let done = fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(named)
        .and_then(|()| write_pid_file(run))
        .and_then(|()| {
            pid_written = true;
            let mut line = path.as_os_str().as_bytes().to_vec();
            line.push(b'\n');
            write_run_file(&run.socket_record(), &line)
        });
    if let Err(e) = done {
        // the socket we just bound, and its pid file; no daemon will serve them
        drop(listener);
        let _ = fs::remove_file(&path);
        if pid_written {
            let _ = fs::remove_file(run.pid_file());
        }
        return Err(e);
    }
    Ok((listener, path))
}

/// `<pid>\n`, mode 0o600, written atomically (`write_run_file`).
fn write_pid_file(run: &RunDir) -> io::Result<()> {
    write_run_file(
        &run.pid_file(),
        format!("{}\n", std::process::id()).as_bytes(),
    )
}

/// Writes `bytes` to the run-dir file `target` (mode 0o600): to `<target>.tmp`, never through a
/// symlink, then renamed into place.
fn write_run_file(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = target.with_file_name(format!(
        "{}.tmp",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    // never through a symlink: a planted `<name>.tmp` link must not get its target written
    let mut f = open_no_follow(
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600),
        &tmp,
    )?;
    // a leftover tmp file keeps its old mode through open; set it exactly
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    drop(f);
    fs::rename(&tmp, target)
}

/// The effective uid of the process at the other end of `stream`.
#[allow(unsafe_code)]
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "linux")]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: `fd` is a live socket owned by `stream`; `cred` and `len` are valid for writes
        // and `len` holds the size of `cred`.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: `fd` is a live socket owned by `stream`; `uid` and `gid` are valid for writes.
        let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

/// This process's effective uid.
#[allow(unsafe_code)]
pub fn own_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}
