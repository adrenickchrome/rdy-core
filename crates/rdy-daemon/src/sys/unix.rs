//! The Unix side of `sys`: every item, each body the code it replaces.

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use tokio::net::{UnixListener, UnixStream};

// 1. Transport

/// The daemon's listener: a Unix socket.
pub type Listener = UnixListener;
/// One connection to or from the daemon.
pub type Stream = UnixStream;
/// The read half of a `Stream`.
pub type ReadHalf = tokio::net::unix::OwnedReadHalf;
/// The write half of a `Stream`.
pub type WriteHalf = tokio::net::unix::OwnedWriteHalf;

/// Binds a listener at `path`.
pub fn bind(path: &Path) -> io::Result<Listener> {
    UnixListener::bind(path)
}

/// Connects to the listener at `path`.
pub async fn connect(path: &Path) -> io::Result<Stream> {
    UnixStream::connect(path).await
}

/// Splits `stream` into halves that can move to different tasks.
pub fn split(stream: Stream) -> (ReadHalf, WriteHalf) {
    stream.into_split()
}

/// The bytes of `p`, as the operating system holds them.
pub fn path_bytes(p: &Path) -> &[u8] {
    p.as_os_str().as_bytes()
}

/// The path whose bytes are `b` (the inverse of `path_bytes`).
pub fn path_from_bytes(b: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(b))
}

// 2. Peer identity

/// The effective uid of the process at the other end of `stream`.
#[allow(unsafe_code)]
pub fn peer_uid(stream: &Stream) -> io::Result<u32> {
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

/// The pid of the process at the other end of `stream`, `None` when the system does not say.
pub fn peer_pid(stream: &Stream) -> Option<u32> {
    stream
        .peer_cred()
        .ok()
        .and_then(|c| c.pid())
        .and_then(|p| u32::try_from(p).ok())
}

/// This process's effective uid.
#[allow(unsafe_code)]
pub fn own_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

// 3. No-follow opens

/// Opens `path` with `opts` and `O_NOFOLLOW`: a run-dir file that is a symlink is refused, never
/// followed, so a checkout cannot point `.rdy/run/<file>` at a file elsewhere for rdy to write,
/// chmod or read.
pub fn open_no_follow(opts: &mut OpenOptions, path: &Path) -> io::Result<File> {
    match opts.custom_flags(libc::O_NOFOLLOW).open(path) {
        Ok(f) => Ok(f),
        Err(e)
            if e.raw_os_error() == Some(libc::ELOOP)
                && fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) =>
        {
            Err(io::Error::other(format!(
                "{} is a symbolic link, which rdy does not follow",
                path.display()
            )))
        }
        Err(e) => Err(e),
    }
}

// 4. Private files and directories

/// Makes a file that `opts` creates private to this user (mode 0o600).
pub fn private_file(opts: &mut OpenOptions) -> &mut OpenOptions {
    opts.mode(0o600)
}

/// Sets the open file `f` to mode 0o600: a file left by an older run keeps its mode through
/// open.
pub fn make_private_file(f: &File) -> io::Result<()> {
    f.set_permissions(fs::Permissions::from_mode(0o600))
}

/// Sets the file at `path` (the socket) to mode 0o600.
pub fn make_private_path(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

/// Creates the directory `path` with mode 0o700; an existing `path` is an error, as with
/// `DirBuilder::create`.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

/// Sets the directory `path` to mode 0o700.
pub fn make_private_dir(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Whether `meta`'s mode is exactly 0o700.
pub fn is_private_dir(meta: &Metadata) -> bool {
    meta.permissions().mode() & 0o777 == 0o700
}

/// The uid that owns the file `meta` describes.
pub fn owner(meta: &Metadata) -> u32 {
    meta.uid()
}

/// Whether `e` says the file system has no hard links (`EPERM`, `ENOTSUP`, `EOPNOTSUPP`).
pub fn no_hard_links(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(n) if n == libc::EPERM || n == libc::ENOTSUP || n == libc::EOPNOTSUPP
    )
}

// 5. Process lifecycle

/// Gives the child that `cmd` spawns a new session (so a new process group) of its own, made in
/// the child before its exec.
#[allow(unsafe_code)]
pub fn new_session_on_spawn(cmd: &mut Command) {
    // A new session, made in the child before its exec. `process_group(0)` alone would make the
    // child a process-group leader, and a group leader's `setsid` fails with EPERM, so the
    // daemon's own `detach` could never leave our session.
    // SAFETY: the closure calls only `setsid`, which is async-signal-safe, and allocates nothing.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// The exit code of `status`: its code, or `128 + n` when signal `n` ended it, as a shell
/// reports it.
pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// Whether a process `pid` exists (`kill(pid, 0)`; a process of another user counts).
#[allow(unsafe_code)]
pub fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; `kill` only checks that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Sends `SIGTERM` to `pid`, which `rdy serve` takes as `rdy serve --stop`. `Ok(true)` when it
/// was sent, `Ok(false)` when the process is gone (`ESRCH`). A `pid` of 1 or less is refused:
/// never init, and never a process group (`kill(0, ..)` and negative pids).
#[allow(unsafe_code)]
pub fn terminate(pid: u32) -> io::Result<bool> {
    if pid <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal process {pid}"),
        ));
    }
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("bad pid {pid}")))?;
    // SAFETY: `kill` takes two integers and touches no memory of ours; `pid` is above 1, so it
    // names one process.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(e)
    }
}

/// Leaves the caller's session (`setsid`). `EPERM` means the caller already leads a process
/// group (a child of `serve_command` already leads its own session), and is `Ok`. Called by
/// `rdy serve --detach` after its argument parsing and before it opens the store.
#[allow(unsafe_code)]
pub fn detach() -> io::Result<()> {
    // SAFETY: `setsid` takes no arguments and touches no memory of ours.
    if unsafe { libc::setsid() } == -1 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EPERM) {
            return Err(e);
        }
    }
    Ok(())
}

/// The caller's process group id (`getpgid(0)`), for the tests.
#[doc(hidden)]
#[allow(unsafe_code)]
pub fn own_pgid() -> i32 {
    // SAFETY: `getpgid(0)` asks about the caller and touches no memory of ours.
    unsafe { libc::getpgid(0) }
}

/// The caller's session id (`getsid(0)`), for the tests.
#[doc(hidden)]
#[allow(unsafe_code)]
pub fn own_sid() -> i32 {
    // SAFETY: `getsid(0)` asks about the caller and touches no memory of ours.
    unsafe { libc::getsid(0) }
}

// 6. Stop signals

/// SIGINT and SIGTERM, registered in the current tokio runtime.
pub struct StopSignals {
    int: tokio::signal::unix::Signal,
    term: tokio::signal::unix::Signal,
}

impl StopSignals {
    /// Registers the handlers in the current tokio runtime; call it inside one.
    pub fn install() -> io::Result<StopSignals> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(StopSignals {
            int: signal(SignalKind::interrupt())?,
            term: signal(SignalKind::terminate())?,
        })
    }

    /// Waits for the next signal and names it: "SIGINT" or "SIGTERM".
    pub async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.int.recv() => "SIGINT",
            _ = self.term.recv() => "SIGTERM",
        }
    }
}

// 7. The caller's SSH agent

/// A blocking connection to the caller's SSH agent (the socket its `SSH_AUTH_SOCK` names). It
/// is used only on a request's blocking worker, never on the daemon's writer, and lives for one
/// signing call.
pub type AgentStream = std::os::unix::net::UnixStream;

/// Connects to the SSH agent socket at `path`.
pub fn agent_connect(path: &Path) -> io::Result<AgentStream> {
    AgentStream::connect(path)
}

/// A second handle on `stream`, so another thread can close it while a read waits.
pub fn agent_handle(stream: &AgentStream) -> io::Result<AgentStream> {
    stream.try_clone()
}

/// Closes `stream` both ways: a read or write that waits on it, through any handle, ends. An
/// error (the stream is already closed) is ignored.
pub fn agent_close(stream: &AgentStream) {
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use crate::sys::{
        StopSignals, alive, bind, connect, create_private_dir, exit_code, is_private_dir,
        make_private_dir, make_private_file, make_private_path, no_hard_links, open_no_follow,
        own_uid, owner, path_bytes, path_from_bytes, peer_pid, peer_uid, private_file, split,
        terminate,
    };
    use std::fs::{self, OpenOptions};
    use std::io;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::{Command, ExitStatus};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn ps6_a_bound_listener_accepts_a_connect_and_the_peer_is_this_process() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.sock");
        rt().block_on(async {
            let listener = bind(&path).unwrap();
            let (client, accepted) = tokio::join!(connect(&path), listener.accept());
            let client = client.unwrap();
            let (server, _) = accepted.unwrap();
            for s in [&client, &server] {
                assert_eq!(peer_uid(s).unwrap(), own_uid());
                assert_eq!(peer_pid(s), Some(std::process::id()));
            }
            let (_cr, mut cw) = split(client);
            let (mut sr, _sw) = split(server);
            cw.write_all(b"hi\n").await.unwrap();
            let mut got = [0u8; 3];
            sr.read_exact(&mut got).await.unwrap();
            assert_eq!(&got, b"hi\n");
        });
    }

    #[test]
    fn ps6_path_bytes_round_trip_a_non_utf8_path() {
        let raw: &[u8] = b"/tmp/rdy-\xff\xfe/rdy.sock";
        let p = path_from_bytes(raw);
        assert!(p.to_str().is_none(), "the path is not UTF-8");
        assert_eq!(path_bytes(&p), raw);
        assert_eq!(path_bytes(Path::new("/a/b.sock")), b"/a/b.sock");
    }

    #[test]
    fn ps6_open_no_follow_refuses_a_symlink_with_its_text() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("target");
        fs::write(&target, b"x").unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let e = open_no_follow(OpenOptions::new().read(true), &link).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "{} is a symbolic link, which rdy does not follow",
                link.display()
            )
        );
        // a plain file opens
        open_no_follow(OpenOptions::new().read(true), &target).unwrap();
    }

    /// The umask the child case runs under, in octal, and the directory it works in. The
    /// parent test sets both; without them the child case does nothing.
    const CHILD_UMASK: &str = "RDY_PS6_CHILD_UMASK";
    const CHILD_DIR: &str = "RDY_PS6_CHILD_DIR";

    #[test]
    fn ps6_private_files_and_dirs_get_0600_and_0700_whatever_the_umask() {
        // Each umask runs in its own process, so it touches no other test, and is set by
        // `sh`, so the test needs no `unsafe`. 000 masks nothing: a creation mode wider than
        // 0o600 or 0o700 shows. 077 is the usual umask. 277 masks the owner's write and
        // execute bits: only the make_private_* calls give the exact modes.
        let name = format!(
            "{}::ps6_private_modes_under_the_child_umask",
            module_path!().split_once("::").unwrap().1
        );
        let exe = std::env::current_exe().unwrap();
        for umask in ["000", "077", "277"] {
            let d = tempfile::tempdir().unwrap();
            // the fixture directory stays usable whatever umask this process has
            fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let out = Command::new("/bin/sh")
                .args(["-c", r#"umask "$1" && shift && exec "$@""#, "sh", umask])
                .arg(&exe)
                .args([&name, "--exact", "--test-threads=1", "--nocapture"])
                .env(CHILD_UMASK, umask)
                .env(CHILD_DIR, d.path())
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.contains("test result: ok. 1 passed"),
                "umask {umask}: {}\n{stdout}\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// The child case of the test above: it runs only in the process that test starts.
    #[test]
    fn ps6_private_modes_under_the_child_umask() {
        let (Some(umask), Some(dir)) = (std::env::var_os(CHILD_UMASK), std::env::var_os(CHILD_DIR))
        else {
            return;
        };
        let umask = u32::from_str_radix(umask.to_str().unwrap(), 8).unwrap();
        let d = Path::new(&dir);
        // the umask is in effect
        let probe = d.join("probe");
        drop(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o666)
                .open(&probe)
                .unwrap(),
        );
        assert_eq!(mode(&probe), 0o666 & !umask, "the umask is {umask:03o}");
        // a new file opened private: 0o600 less the umask, then exactly 0o600
        let new = d.join("new");
        let f = private_file(OpenOptions::new().write(true).create_new(true))
            .open(&new)
            .unwrap();
        assert_eq!(
            mode(&new),
            0o600 & !umask,
            "creation mode, umask {umask:03o}"
        );
        make_private_file(&f).unwrap();
        assert_eq!(mode(&new), 0o600);
        drop(f);
        // an existing 0o644 file, through its handle
        let open = d.join("open");
        fs::write(&open, b"x").unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o644)).unwrap();
        let f = OpenOptions::new().write(true).open(&open).unwrap();
        make_private_file(&f).unwrap();
        assert_eq!(mode(&open), 0o600);
        // an existing 0o644 file, by its path (the socket)
        let by_path = d.join("by-path");
        fs::write(&by_path, b"x").unwrap();
        fs::set_permissions(&by_path, fs::Permissions::from_mode(0o644)).unwrap();
        make_private_path(&by_path).unwrap();
        assert_eq!(mode(&by_path), 0o600);
        // a new directory: 0o700 less the umask, then exactly 0o700
        let dir = d.join("dir");
        create_private_dir(&dir).unwrap();
        assert_eq!(
            mode(&dir),
            0o700 & !umask,
            "creation mode, umask {umask:03o}"
        );
        assert_eq!(
            is_private_dir(&fs::symlink_metadata(&dir).unwrap()),
            umask & 0o700 == 0
        );
        make_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert!(is_private_dir(&fs::symlink_metadata(&dir).unwrap()));
        // an existing 0o755 directory
        let wide = d.join("wide");
        fs::create_dir(&wide).unwrap();
        fs::set_permissions(&wide, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!is_private_dir(&fs::symlink_metadata(&wide).unwrap()));
        make_private_dir(&wide).unwrap();
        assert_eq!(mode(&wide), 0o700);
        assert!(is_private_dir(&fs::symlink_metadata(&wide).unwrap()));
        // create_private_dir refuses an existing directory, as DirBuilder::create does
        assert_eq!(
            create_private_dir(&wide).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn ps6_owner_is_this_user() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(owner(&fs::symlink_metadata(d.path()).unwrap()), own_uid());
    }

    #[test]
    fn ps6_no_hard_links_matches_eperm_enotsup_eopnotsupp_only() {
        for n in [libc::EPERM, libc::ENOTSUP, libc::EOPNOTSUPP] {
            assert!(no_hard_links(&io::Error::from_raw_os_error(n)), "errno {n}");
        }
        for n in [
            libc::EACCES,
            libc::ENOENT,
            libc::EEXIST,
            libc::EXDEV,
            libc::EMLINK,
        ] {
            assert!(
                !no_hard_links(&io::Error::from_raw_os_error(n)),
                "errno {n}"
            );
        }
        assert!(!no_hard_links(&io::Error::other("no errno")));
    }

    #[test]
    fn ps6_alive_and_terminate_follow_a_child() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        assert!(alive(pid));
        assert!(terminate(pid).unwrap());
        let status = child.wait().unwrap();
        assert_eq!(exit_code(status), 128 + libc::SIGTERM);
        // reaped by the wait, so the pid names no process
        assert!(!alive(pid));
        assert!(!terminate(pid).unwrap());
    }

    #[test]
    fn ps6_terminate_refuses_pid_0_and_1_with_its_text() {
        for pid in [0, 1] {
            let e = terminate(pid).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(e.to_string(), format!("refusing to signal process {pid}"));
        }
    }

    #[test]
    fn ps6_exit_code_is_128_plus_the_signal() {
        // a raw wait status: the low 7 bits name the signal, the next byte the exit code
        assert_eq!(exit_code(ExitStatus::from_raw(libc::SIGKILL)), 128 + 9);
        assert_eq!(exit_code(ExitStatus::from_raw(libc::SIGTERM)), 128 + 15);
        assert_eq!(exit_code(ExitStatus::from_raw(3 << 8)), 3);
        assert_eq!(exit_code(ExitStatus::from_raw(0)), 0);
    }

    #[test]
    fn sig3_agent_stream_connects_and_close_wakes_a_blocked_read() {
        use crate::sys::{agent_close, agent_connect, agent_handle};
        use std::io::{Read, Write};
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("agent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let (got_tx, got_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut req = [0u8; 5];
            conn.read_exact(&mut req).unwrap();
            got_tx.send(req).unwrap();
            // hold the connection open without replying until the client closes it
            let mut rest = Vec::new();
            conn.read_to_end(&mut rest).unwrap();
        });
        let mut stream = agent_connect(&path).unwrap();
        let handle = agent_handle(&stream).unwrap();
        stream.write_all(&[0, 0, 0, 1, 11]).unwrap();
        assert_eq!(got_rx.recv().unwrap(), [0, 0, 0, 1, 11]);
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4];
            stream.read(&mut buf)
        });
        // closing through the second handle ends the blocked read: no reply arrives
        agent_close(&handle);
        assert!(matches!(reader.join().unwrap(), Ok(0) | Err(_)));
        server.join().unwrap();
        // no listener at a path: connect fails
        assert!(agent_connect(&d.path().join("none.sock")).is_err());
    }

    #[test]
    fn ps6_stop_signals_install_in_a_runtime() {
        let rt = rt();
        let _g = rt.enter();
        StopSignals::install().unwrap();
    }
}
