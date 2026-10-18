//! The Unix side of `sys`: every item, each body the code it replaces.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
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

/// This process's effective uid (moved to `rdy_store::pio`, re-exported here).
pub use rdy_store::pio::own_uid;

// 3. No-follow opens

/// Opening with `O_NOFOLLOW` (moved to `rdy_store::pio`, re-exported here): a run-dir file that
/// is a symlink is refused, never followed.
pub use rdy_store::pio::open_no_follow;

// 4. Private files and directories

/// Private files and directories, and a file's owner (moved to `rdy_store::pio`, re-exported
/// here): modes 0o600 and 0o700.
pub use rdy_store::pio::{
    create_private_dir, is_private_dir, make_private_dir, make_private_file, owner, private_file,
};

/// Sets the file at `path` (the socket) to mode 0o600.
pub fn make_private_path(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
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

/// How long a connect waits before it tries again when the agent's listen queue is full (Linux
/// refuses a non-blocking connect then, with `EAGAIN`). It is a retry pace, not a timeout: the
/// connect goes on until it connects, fails or is cancelled.
const AGENT_CONNECT_RETRY_MS: libc::c_int = 20;

/// Ends a pending [`agent_connect`] from another thread. Once set, it stays set.
pub struct AgentCancel {
    /// Shut down to set the cancel.
    trigger: std::os::unix::net::UnixStream,
    /// Readable (at its end) once the cancel is set.
    watch: std::os::unix::net::UnixStream,
}

impl AgentCancel {
    /// A new cancel, not set.
    pub fn new() -> io::Result<AgentCancel> {
        let (trigger, watch) = std::os::unix::net::UnixStream::pair()?;
        Ok(AgentCancel { trigger, watch })
    }

    /// Sets the cancel: a connect that waits with it ends as `Interrupted`. Setting it again
    /// does nothing.
    pub fn cancel(&self) {
        let _ = self.trigger.shutdown(std::net::Shutdown::Write);
    }

    /// Whether the cancel is set.
    pub fn is_set(&self) -> bool {
        let mut fds = [self.watch_fd()];
        matches!(poll_fds(&mut fds, 0), Ok(n) if n > 0)
    }

    /// Waits until the cancel is set (or `poll` fails, which it does not on a live socket).
    pub fn wait(&self) {
        let mut fds = [self.watch_fd()];
        let _ = poll_fds(&mut fds, -1);
    }

    fn watch_fd(&self) -> libc::pollfd {
        libc::pollfd {
            fd: self.watch.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }
    }
}

/// `poll` on `fds` for at most `timeout_ms` (-1 waits without end), again after a signal.
#[allow(unsafe_code)]
fn poll_fds(fds: &mut [libc::pollfd], timeout_ms: libc::c_int) -> io::Result<libc::c_int> {
    let n = libc::nfds_t::try_from(fds.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    loop {
        // SAFETY: `fds` is a live, writable array of `n` pollfd records for the whole call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), n, timeout_ms) };
        if r >= 0 {
            return Ok(r);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// The error of a cancelled connect.
fn agent_cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "agent connect cancelled")
}

/// The socket address of `path`, and its length.
#[allow(unsafe_code)]
fn unix_addr(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: `sockaddr_un` is plain integers and bytes, for which all zeros is a valid value.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    // One byte stays for the terminating NUL.
    if bytes.contains(&0) || bytes.len() >= addr.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "agent socket path is too long or holds a NUL byte",
        ));
    }
    addr.sun_family =
        libc::sa_family_t::try_from(libc::AF_UNIX).map_err(|_| io::ErrorKind::InvalidInput)?;
    for (to, from) in addr.sun_path.iter_mut().zip(bytes) {
        *to = libc::c_char::from_ne_bytes([*from]);
    }
    let len = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        addr.sun_len = u8::try_from(len).map_err(|_| io::ErrorKind::InvalidInput)?;
    }
    let len = libc::socklen_t::try_from(len).map_err(|_| io::ErrorKind::InvalidInput)?;
    Ok((addr, len))
}

/// A new non-blocking, close-on-exec Unix stream socket.
///
/// Where the platform has `SOCK_CLOEXEC`, the socket is close-on-exec from its creation, so a
/// child that another thread forks at any moment never gets it. Elsewhere (macOS) the flag is set
/// right after creation, as the standard library does there: such a platform has no atomic way.
#[allow(unsafe_code)]
fn nonblocking_unix_socket() -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "illumos"
    ))]
    {
        // SAFETY: `socket` takes three integers and touches no memory of ours.
        let fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a new descriptor that nothing else owns; the OwnedFd closes it.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        #[cfg(test)]
        tests::after_agent_socket(fd.as_raw_fd());
        Ok(fd)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "illumos"
    )))]
    {
        // SAFETY: `socket` takes three integers and touches no memory of ours.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a new descriptor that nothing else owns; the OwnedFd closes it.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        #[cfg(test)]
        tests::after_agent_socket(fd.as_raw_fd());
        // SAFETY: `fcntl` with F_SETFD takes the open descriptor and integers.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fcntl` with F_GETFL takes the open descriptor and touches no memory of ours.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fcntl` with F_SETFL takes the open descriptor and integers.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd)
    }
}

/// One `connect` of the socket `fd` to `addr` (`len` bytes long).
#[allow(unsafe_code)]
fn connect_once(
    fd: &std::os::fd::OwnedFd,
    addr: &libc::sockaddr_un,
    len: libc::socklen_t,
) -> io::Result<()> {
    // SAFETY: `addr` is a valid socket address of at least `len` bytes for the whole call.
    let r = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            std::ptr::from_ref(addr).cast::<libc::sockaddr>(),
            len,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Connects to the SSH agent socket at `path`, on the caller's thread. The connect waits until
/// it connects or fails, or until `cancel` is set from another thread: then it ends as
/// `Interrupted`, and its socket is closed before it returns. A connect never runs on after
/// its call: there is no helper thread.
#[allow(unsafe_code)] // getsockopt
pub fn agent_connect(path: &Path, cancel: &AgentCancel) -> io::Result<AgentStream> {
    let (addr, len) = unix_addr(path)?;
    let fd = nonblocking_unix_socket()?;
    loop {
        if cancel.is_set() {
            return Err(agent_cancelled());
        }
        let Err(e) = connect_once(&fd, &addr, len) else {
            break;
        };
        match e.raw_os_error() {
            // The connect goes on in the background: wait for it, or for the cancel.
            Some(libc::EINPROGRESS | libc::EALREADY | libc::EINTR) => {
                let mut fds = [
                    libc::pollfd {
                        fd: fd.as_raw_fd(),
                        events: libc::POLLOUT,
                        revents: 0,
                    },
                    cancel.watch_fd(),
                ];
                poll_fds(&mut fds, -1)?;
                if fds[1].revents != 0 {
                    return Err(agent_cancelled());
                }
                let mut err: libc::c_int = 0;
                let mut err_len = libc::socklen_t::try_from(std::mem::size_of::<libc::c_int>())
                    .map_err(|_| io::ErrorKind::InvalidInput)?;
                // SAFETY: `err` and `err_len` are live, writable and sized for SO_ERROR.
                let r = unsafe {
                    libc::getsockopt(
                        fd.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        (&raw mut err).cast::<libc::c_void>(),
                        &mut err_len,
                    )
                };
                if r < 0 {
                    return Err(io::Error::last_os_error());
                }
                if err != 0 {
                    return Err(io::Error::from_raw_os_error(err));
                }
                break;
            }
            // A connect interrupted earlier has finished meanwhile.
            Some(libc::EISCONN) => break,
            // The listen queue is full (Linux): try again, unless the cancel comes first.
            Some(libc::EAGAIN) => {
                let mut fds = [cancel.watch_fd()];
                if poll_fds(&mut fds, AGENT_CONNECT_RETRY_MS)? > 0 {
                    return Err(agent_cancelled());
                }
            }
            _ => return Err(e),
        }
    }
    let stream = AgentStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
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

// 8. The caller's interrupt while it signs

/// The write end of the pipe the `SIGINT` handler writes to; -1 until the pipe exists. It is
/// never closed, so the handler never writes to a reused descriptor.
static INTERRUPT_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// Who catches `SIGINT` now.
struct InterruptState {
    /// The guards alive; the handler is installed while there is one.
    holders: usize,
    /// The action the first guard replaced, put back by the last.
    previous: Option<libc::sigaction>,
    /// Counts the interrupts, for every guard; made with the pipe, by the first guard.
    count: Option<tokio::sync::watch::Sender<u64>>,
}

static INTERRUPT_STATE: std::sync::Mutex<InterruptState> = std::sync::Mutex::new(InterruptState {
    holders: 0,
    previous: None,
    count: None,
});

fn interrupt_state() -> std::sync::MutexGuard<'static, InterruptState> {
    INTERRUPT_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `SIGINT` caught while the guard lives: [`InterruptGuard::recv`] ends at the next one. With no
/// guard alive, `SIGINT` has the action it had before the first guard.
pub struct InterruptGuard {
    seen: tokio::sync::watch::Receiver<u64>,
}

impl InterruptGuard {
    /// Waits for a `SIGINT` caught since the guard was made.
    pub async fn recv(&mut self) {
        if self.seen.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        let mut st = interrupt_state();
        st.holders = st.holders.saturating_sub(1);
        if st.holders == 0
            && let Some(previous) = st.previous.take()
        {
            // SAFETY: `previous` is the action `sigaction` reported for SIGINT; putting it back
            // touches no memory of ours.
            #[allow(unsafe_code)]
            unsafe {
                libc::sigaction(libc::SIGINT, &previous, std::ptr::null_mut());
            }
        }
    }
}

/// The `SIGINT` handler: one byte on the pipe. It does only async-signal-safe work and keeps
/// `errno`.
extern "C" fn on_interrupt(_signal: libc::c_int) {
    let fd = INTERRUPT_FD.load(std::sync::atomic::Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    // SAFETY: `errno_location` returns this thread's errno; `write` is async-signal-safe and
    // writes one byte from a live buffer to a non-blocking descriptor that is never closed.
    #[allow(unsafe_code)]
    unsafe {
        let errno = errno_location();
        let saved = *errno;
        libc::write(fd, [1u8].as_ptr().cast(), 1);
        *errno = saved;
    }
}

/// This thread's `errno`.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
#[allow(unsafe_code)]
unsafe fn errno_location() -> *mut libc::c_int {
    // SAFETY: returns the caller's thread's errno; no other effect.
    unsafe { libc::__error() }
}

/// This thread's `errno`.
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
#[allow(unsafe_code)]
unsafe fn errno_location() -> *mut libc::c_int {
    // SAFETY: returns the caller's thread's errno; no other effect.
    unsafe { libc::__errno_location() }
}

/// Makes the pipe, and the thread that counts what the handler writes to it.
fn interrupt_count() -> io::Result<tokio::sync::watch::Sender<u64>> {
    use std::io::Read;
    use std::os::fd::IntoRawFd;
    let (mut read, write) = std::os::unix::net::UnixStream::pair()?;
    write.set_nonblocking(true)?;
    let (count, _) = tokio::sync::watch::channel(0u64);
    let counter = count.clone();
    std::thread::Builder::new()
        .name("rdy-sigint".into())
        .spawn(move || {
            let mut buf = [0u8; 64];
            while let Ok(n) = read.read(&mut buf) {
                if n == 0 {
                    break;
                }
                counter.send_modify(|c| *c = c.wrapping_add(1));
            }
        })?;
    INTERRUPT_FD.store(write.into_raw_fd(), std::sync::atomic::Ordering::Relaxed);
    Ok(count)
}

/// Catches `SIGINT` until the guard is dropped (guards may overlap). Call it before the work an
/// interrupt should stop begins, so an interrupt during that work is never missed.
pub fn catch_interrupt() -> io::Result<InterruptGuard> {
    let mut st = interrupt_state();
    let count = match &st.count {
        Some(c) => c.clone(),
        None => {
            let c = interrupt_count()?;
            st.count = Some(c.clone());
            c
        }
    };
    let seen = count.subscribe();
    if st.holders == 0 {
        // SAFETY: zeroed `sigaction` structs are valid; the new action names a handler that does
        // only async-signal-safe work, and the old action is written into memory we own.
        #[allow(unsafe_code)]
        let previous = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            let mut previous: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(libc::SIGINT, &action, &mut previous) != 0 {
                return Err(io::Error::last_os_error());
            }
            previous
        };
        st.previous = Some(previous);
    }
    st.holders += 1;
    Ok(InterruptGuard { seen })
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

    /// Lowers the listen backlog of `listener` to 1, so a few queued connections fill it (macOS
    /// reads a backlog of 0 as the system maximum).
    #[allow(unsafe_code)]
    fn lower_backlog(listener: &std::os::unix::net::UnixListener) {
        use std::os::fd::AsRawFd;
        // SAFETY: `listen` takes the listener's own open descriptor and an integer.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
    }

    /// The connections waiting in `listener`'s queue, accepted until none is left.
    fn drain(listener: &std::os::unix::net::UnixListener) -> usize {
        listener.set_nonblocking(true).unwrap();
        let mut n = 0;
        loop {
            match listener.accept() {
                Ok(_) => n += 1,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return n,
                Err(e) => panic!("accept: {e}"),
            }
        }
    }

    #[test]
    fn sig3_agent_connect_cancel_ends_a_pending_connect_and_drops_its_socket() {
        use crate::sys::{AgentCancel, agent_connect};
        use std::os::unix::net::UnixListener;
        use std::sync::Arc;
        use std::sync::mpsc::channel;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("agent.sock");
        let listener = UnixListener::bind(&path).unwrap();

        // A cancel set before the connect: nothing reaches the listener.
        let cancel = AgentCancel::new().unwrap();
        cancel.cancel();
        let e = agent_connect(&path, &cancel).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted);
        assert_eq!(drain(&listener), 0, "a cancelled connect never connects");

        // Fill the listen queue with non-blocking connects until one is turned away. On Linux a
        // further connect then waits for room; on macOS it is refused at once (a Unix connect
        // never waits there).
        lower_backlog(&listener);
        let (addr, len) = super::unix_addr(&path).unwrap();
        let mut held = Vec::new();
        while held.len() < 4096 {
            let fd = super::nonblocking_unix_socket().unwrap();
            if super::connect_once(&fd, &addr, len).is_err() {
                break;
            }
            held.push(fd);
        }
        // Repeated cancelled attempts: each ends, and none is left to connect later.
        for _ in 0..3 {
            let cancel = Arc::new(AgentCancel::new().unwrap());
            let (started_tx, started) = channel();
            let attempt = {
                let cancel = Arc::clone(&cancel);
                let path = path.clone();
                std::thread::spawn(move || {
                    started_tx.send(()).unwrap();
                    agent_connect(&path, &cancel)
                })
            };
            started.recv().unwrap();
            cancel.cancel();
            let e = attempt.join().unwrap().unwrap_err();
            if cfg!(target_os = "linux") {
                assert_eq!(e.kind(), io::ErrorKind::Interrupted);
            } else {
                assert!(matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::ConnectionRefused
                ));
            }
        }
        // Only the connections made to fill the queue are in it: the cancelled attempts' sockets
        // are gone, so none connects once there is room.
        assert_eq!(drain(&listener), held.len());
        drop(held);
        assert_eq!(drain(&listener), 0);
        // With room, an uncancelled connect succeeds.
        let _c = agent_connect(&path, &AgentCancel::new().unwrap()).unwrap();
        assert_eq!(drain(&listener), 1);
    }

    #[test]
    fn sig3_agent_stream_connects_and_close_wakes_a_blocked_read() {
        use crate::sys::{AgentCancel, agent_close, agent_connect, agent_handle};
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
        let mut stream = agent_connect(&path, &AgentCancel::new().unwrap()).unwrap();
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
        // The server's end sees end-of-file once the last handle is gone, as when a call drops
        // its connection. (macOS can miss the write-side shutdown that races a blocked read, so
        // the test does not wait on it while the handle is open.)
        drop(handle);
        server.join().unwrap();
        // no listener at a path: connect fails
        assert!(agent_connect(&d.path().join("none.sock"), &AgentCancel::new().unwrap()).is_err());
    }

    /// A step run on the agent socket's descriptor in a test.
    type SocketHook = Box<dyn FnMut(std::os::fd::RawFd)>;

    thread_local! {
        /// Runs on this thread right after the agent socket is created, before anything else
        /// is done to it.
        static AFTER_AGENT_SOCKET: std::cell::RefCell<Option<SocketHook>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn after_agent_socket(fd: std::os::fd::RawFd) {
        AFTER_AGENT_SOCKET.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f(fd);
            }
        });
    }

    #[test]
    fn sig3_agent_socket_is_non_blocking_and_close_on_exec() {
        use std::os::fd::AsRawFd;
        let fd = super::nonblocking_unix_socket().unwrap();
        // SAFETY: `fcntl` with F_GETFD and F_GETFL takes the open descriptor only.
        #[allow(unsafe_code)]
        let (fd_flags, fl_flags) = unsafe {
            (
                libc::fcntl(fd.as_raw_fd(), libc::F_GETFD),
                libc::fcntl(fd.as_raw_fd(), libc::F_GETFL),
            )
        };
        assert!(fd_flags >= 0 && fl_flags >= 0);
        assert_ne!(fd_flags & libc::FD_CLOEXEC, 0, "close-on-exec");
        assert_ne!(fl_flags & libc::O_NONBLOCK, 0, "non-blocking");
    }

    /// A child process started at any moment after the agent socket exists never gets it: the
    /// socket is close-on-exec from its creation, not from a later step a concurrent fork can
    /// come before.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn sig3_agent_socket_is_never_inherited_by_a_child_started_at_its_creation() {
        use std::cell::Cell;
        use std::rc::Rc;
        let inherited: Rc<Cell<Option<bool>>> = Rc::new(Cell::new(None));
        AFTER_AGENT_SOCKET.with(|h| {
            let inherited = Rc::clone(&inherited);
            *h.borrow_mut() = Some(Box::new(move |fd| {
                let link = fs::read_link(format!("/proc/self/fd/{fd}")).unwrap();
                let link = link.to_str().unwrap().to_owned();
                assert!(link.starts_with("socket:"), "{link}");
                // The child is a shell: it reports whether its own descriptor `fd` is this socket.
                let status = Command::new("sh")
                    .arg("-c")
                    .arg(format!("[ \"$(readlink /proc/$$/fd/{fd})\" = '{link}' ]"))
                    .status()
                    .unwrap();
                inherited.set(Some(status.success()));
            }));
        });
        let fd = super::nonblocking_unix_socket();
        AFTER_AGENT_SOCKET.with(|h| *h.borrow_mut() = None);
        drop(fd.unwrap());
        assert_eq!(
            inherited.get(),
            Some(false),
            "a child started right after the socket's creation holds the agent socket"
        );
    }

    #[test]
    fn ps6_stop_signals_install_in_a_runtime() {
        let rt = rt();
        let _g = rt.enter();
        StopSignals::install().unwrap();
    }
}
