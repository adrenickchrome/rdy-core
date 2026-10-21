//! The harness's Unix-only helpers, in one place (FW-XP1 D-1RXEA).
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

#[cfg(not(unix))]
compile_error!("rdy_qa::platform has no implementation for this platform yet (FW-XP1 Phase 4)");

/// A directory short enough for a socket path under it: `/tmp` (macOS `sun_path` is 104 bytes
/// and its `temp_dir()` is already about 50).
pub fn short_tmp_root() -> &'static Path {
    Path::new("/tmp")
}

/// Whether process `pid` has exited: `ps` lists no such process, or lists a zombie (killed, not
/// yet reaped by its parent).
pub fn process_exited(pid: u32) -> bool {
    match Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) => {
            let stat = String::from_utf8_lossy(&o.stdout);
            let stat = stat.trim();
            !o.status.success() || stat.is_empty() || stat.starts_with('Z')
        }
        Err(e) => panic!("cannot run ps: {e}"),
    }
}

/// Whether process `pid` is alive (`kill -0` succeeds).
pub fn process_running(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// SIGKILLs process `pid` (`kill -9`); panics if the command cannot run or fails.
pub fn kill_hard(pid: u32) {
    let status = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -9 {pid} failed");
}

/// Puts the child `cmd` starts in its own process group.
pub fn own_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

/// Opens `path` for append, creating it with mode 0o600.
pub fn open_private_log(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
}

/// Makes `link` a symbolic link to `target`.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Sets the permission bits of `path` to `mode` (for example `0o555` for a read-only directory).
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// The permission bits of `path` (`mode & 0o7777`), following a symlink.
pub fn mode_of(path: &Path) -> io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o7777)
}

/// The output cap of a release tool's child process (the version probe, tar, `uname`).
pub const RELEASE_CHILD_OUTPUT_MAX: usize = 1_048_576;
/// The hang guard of a release tool's child process.
pub const RELEASE_CHILD_DEADLINE: Duration = Duration::from_secs(30);

/// SIGKILLs every process in the group `pid` leads: a group that [`own_process_group`] made.
/// A group with no member left is already stopped, which is success. PID 0 (the caller's own
/// group) and a PID outside the signed range are refused.
pub fn kill_process_group(pid: u32) -> io::Result<()> {
    if pid == 0 || i32::try_from(pid).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{pid} is not a process group this process started"),
        ));
    }
    let group = format!("-{pid}");
    let signal = |sig: &str| {
        Command::new("kill")
            .args(["-s", sig, "--", &group])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    };
    if signal("KILL")?.success() || !signal("0")?.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "process group {pid} is still running and could not be killed"
    )))
}

/// What the bounded runner's workers report to it.
enum RunEvent {
    /// The output passed the cap.
    Overflow,
    /// A pipe could not be read.
    PipeError(io::Error),
    /// A drain reached the end of its pipe (or stopped).
    DrainDone,
    /// The direct child ended (or could not be waited on).
    Exited(io::Result<ExitStatus>),
    /// The deadline passed: the runner's own timeout, or a test's [`Expiry`].
    Expired,
}

/// Ends a bounded run as its deadline would: the runner's internal wait path, for tests.
pub(crate) struct Expiry(mpsc::Sender<RunEvent>);

impl Expiry {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn expire(self) {
        let _ = self.0.send(RunEvent::Expired);
    }
}

/// One bounded run: its result and what its tests check about the cleanup.
pub(crate) struct BoundedRun {
    pub(crate) result: io::Result<Output>,
    /// The bytes kept from both streams together (at most the cap).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) retained: usize,
    /// The drain workers joined: 2 once both streams were read to the end or stopped.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) drains_joined: usize,
    /// The direct child's PID, which is also its process group's ID, once it started.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) pid: Option<u32>,
}

/// Runs `command` in a process group of its own, with stdin from `stdin` (null when `None`)
/// and stdout and stderr captured under one combined cap of `max_output` bytes (tb-A4DAV).
///
/// When the direct child exits, the group is stopped, so a descendant that still holds a pipe
/// cannot keep a drain running. On overflow, at `deadline`, on a pipe error or a setup failure,
/// the group is stopped too; the runner always waits for the direct child and joins both drains
/// before it returns. Overflow is `InvalidData`, the deadline `TimedOut`, and a setup, pipe or
/// cleanup failure keeps its own kind; a failed cleanup is never reported as success. A nonzero
/// exit status is an `Output`, for the caller to judge.
pub fn run_bounded(
    command: Command,
    stdin: Option<File>,
    max_output: usize,
    deadline: Duration,
) -> io::Result<Output> {
    let until = Instant::now().checked_add(deadline);
    run_bounded_inner(command, stdin, max_output, until, drop).result
}

/// [`run_bounded`] with an absolute deadline (`None`: none) and `on_start`, which receives the
/// run's [`Expiry`] once the child and its workers started.
pub(crate) fn run_bounded_inner(
    mut command: Command,
    stdin: Option<File>,
    max_output: usize,
    deadline: Option<Instant>,
    on_start: impl FnOnce(Expiry),
) -> BoundedRun {
    own_process_group(&mut command);
    command
        .stdin(stdin.map_or_else(Stdio::null, Stdio::from))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return BoundedRun {
                result: Err(e),
                retained: 0,
                drains_joined: 0,
                pid: None,
            };
        }
    };
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    let count = Arc::new(AtomicUsize::new(0));
    let mut failure: Option<io::Error> = None;
    let mut drains = Vec::with_capacity(2);
    for drain in [
        spawn_drain(child.stdout.take(), &count, max_output, &tx),
        spawn_drain(child.stderr.take(), &count, max_output, &tx),
    ] {
        match drain {
            Ok(handle) => drains.push(handle),
            Err(e) => failure = failure.or(Some(e)),
        }
    }
    let child = Arc::new(Mutex::new(child));
    let waiter = {
        let (child, tx) = (child.clone(), tx.clone());
        std::thread::Builder::new()
            .name("rdy-qa-wait".into())
            .spawn(move || {
                let status = match child.lock() {
                    Ok(mut c) => c.wait(),
                    Err(_) => Err(io::Error::other("the child's lock is poisoned")),
                };
                let _ = tx.send(RunEvent::Exited(status));
            })
    };
    let waiter = match waiter {
        Ok(handle) => Some(handle),
        Err(e) => {
            failure = failure.or(Some(e));
            None
        }
    };
    on_start(Expiry(tx.clone()));
    drop(tx);

    let lost = || io::Error::other("the bounded runner lost its workers");
    let mut status: Option<ExitStatus> = None;
    let mut exited = false;
    let mut drains_done = 0;
    if failure.is_none() {
        failure = loop {
            let event = match deadline {
                Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => RunEvent::Expired,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break Some(lost()),
                },
                None => match rx.recv() {
                    Ok(event) => event,
                    Err(_) => break Some(lost()),
                },
            };
            match event {
                RunEvent::Overflow => {
                    break Some(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("the child's output passed {max_output} bytes"),
                    ));
                }
                RunEvent::PipeError(e) => break Some(e),
                RunEvent::Expired => {
                    break Some(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the child passed its deadline",
                    ));
                }
                RunEvent::Exited(result) => {
                    exited = true;
                    match result {
                        // a descendant may still hold a pipe: stop the whole group
                        Ok(s) => match kill_process_group(pid) {
                            Ok(()) => status = Some(s),
                            Err(e) => break Some(e),
                        },
                        Err(e) => break Some(e),
                    }
                }
                RunEvent::DrainDone => drains_done += 1,
            }
            if status.is_some() && drains_done == drains.len() {
                break None;
            }
        };
    }

    let mut cleanup: Option<io::Error> = None;
    if failure.is_some() {
        if let Err(e) = kill_process_group(pid) {
            cleanup = Some(e);
        }
        if !exited {
            let reaped =
                waiter.is_some() && rx.iter().any(|event| matches!(event, RunEvent::Exited(_)));
            if !reaped {
                match child.lock() {
                    Ok(mut c) => {
                        if let Err(e) = c.wait() {
                            cleanup = cleanup.or(Some(e));
                        }
                    }
                    Err(_) => cleanup = cleanup.or(Some(lost())),
                }
            }
        }
    }
    if let Some(waiter) = waiter
        && waiter.join().is_err()
    {
        cleanup = cleanup.or(Some(lost()));
    }
    let mut streams = Vec::with_capacity(2);
    let mut drains_joined = 0;
    for drain in drains {
        match drain.join() {
            Ok(bytes) => {
                drains_joined += 1;
                streams.push(bytes);
            }
            Err(_) => {
                cleanup = cleanup.or(Some(lost()));
                streams.push(Vec::new());
            }
        }
    }
    let retained = streams.iter().map(Vec::len).sum();
    let mut streams = streams.into_iter();
    let (stdout, stderr) = (
        streams.next().unwrap_or_default(),
        streams.next().unwrap_or_default(),
    );
    let result = match (failure.or(cleanup), status) {
        (Some(e), _) => Err(e),
        (None, Some(status)) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        (None, None) => Err(lost()),
    };
    BoundedRun {
        result,
        retained,
        drains_joined,
        pid: Some(pid),
    }
}

/// Starts a drain of `pipe` (an error when the child has no such pipe).
fn spawn_drain<R: Read + Send + 'static>(
    pipe: Option<R>,
    count: &Arc<AtomicUsize>,
    max_output: usize,
    tx: &mpsc::Sender<RunEvent>,
) -> io::Result<std::thread::JoinHandle<Vec<u8>>> {
    let pipe = pipe.ok_or_else(|| io::Error::other("the child has no output pipe"))?;
    let (count, tx) = (count.clone(), tx.clone());
    std::thread::Builder::new()
        .name("rdy-qa-drain".into())
        .spawn(move || drain_bounded(pipe, &count, max_output, &tx))
}

/// Reads `pipe` to its end, keeping each byte only while the count both streams share stays
/// within `max_output`; a byte is counted before it is kept. Reports overflow or a read error,
/// then stops; always reports that it is done.
fn drain_bounded(
    mut pipe: impl Read,
    count: &AtomicUsize,
    max_output: usize,
    tx: &mpsc::Sender<RunEvent>,
) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let before = count.fetch_add(n, Ordering::SeqCst);
                let room = max_output.saturating_sub(before).min(n);
                kept.extend_from_slice(&buf[..room]);
                if before.saturating_add(n) > max_output {
                    let _ = tx.send(RunEvent::Overflow);
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                let _ = tx.send(RunEvent::PipeError(e));
                break;
            }
        }
    }
    let _ = tx.send(RunEvent::DrainDone);
    kept
}

/// The release target triple of this machine: macOS arm64 or Linux (GNU) amd64. Any other
/// machine is `Unsupported`.
pub fn release_target() -> io::Result<&'static str> {
    release_target_of(
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_env = "gnu"),
    )
}

/// The release target of `os`/`arch` (`gnu`: the GNU C library).
fn release_target_of(os: &str, arch: &str, gnu: bool) -> io::Result<&'static str> {
    match (os, arch, gnu) {
        ("macos", "aarch64", _) => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64", true) => Ok("x86_64-unknown-linux-gnu"),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("rdy has no release for {os}/{arch}"),
        )),
    }
}

/// The largest `uname -srm` output the OS probe accepts, its LF included.
const UNAME_MAX: usize = 4096;

/// This machine's OS, kernel release and hardware name: `uname -srm`, run through
/// [`run_bounded`] with the release limits, without its final LF.
pub fn release_os() -> io::Result<String> {
    let mut cmd = Command::new("uname");
    cmd.arg("-srm");
    let out = run_bounded(cmd, None, RELEASE_CHILD_OUTPUT_MAX, RELEASE_CHILD_DEADLINE)?;
    if !out.status.success() {
        return Err(io::Error::other(format!("uname -srm: {}", out.status)));
    }
    os_from_uname(&out.stdout)
}

/// The text of `uname -srm` output: UTF-8, at most [`UNAME_MAX`] bytes, one line ending in LF.
fn os_from_uname(stdout: &[u8]) -> io::Result<String> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, format!("uname -srm {why}"));
    if stdout.len() > UNAME_MAX {
        return Err(bad("printed more than 4096 bytes"));
    }
    let text = std::str::from_utf8(stdout).map_err(|_| bad("printed text that is not UTF-8"))?;
    let line = text
        .strip_suffix('\n')
        .ok_or_else(|| bad("printed no final LF"))?;
    if line.is_empty() || line.contains('\n') {
        return Err(bad("did not print exactly one line"));
    }
    Ok(line.to_string())
}

/// A command that runs the POSIX shell script `script` (`sh -c`), for a stub child process.
pub fn shell_script(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script]);
    cmd
}

/// A command that does nothing and succeeds (`true`).
pub fn no_op_command() -> Command {
    Command::new("true")
}

/// A fake SSH agent that records connections: a Unix socket listener (set its path as
/// `SSH_AUTH_SOCK` for a child) that accepts each connection, counts it and closes it at once,
/// so a client that waits for a reply gets end of file instead of hanging.
pub struct RecordingAgent {
    _dir: tempfile::TempDir,
    path: PathBuf,
    listener: Arc<UnixListener>,
    count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RecordingAgent {
    /// Binds a new socket in a fresh directory under [`short_tmp_root`] and starts accepting.
    pub fn start() -> io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("rdy-agent-")
            .tempdir_in(short_tmp_root())?;
        let path = dir.path().join("agent.sock");
        let listener = Arc::new(UnixListener::bind(&path)?);
        listener.set_nonblocking(true)?;
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (listener, count, stop) = (listener.clone(), count.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if !accept_pending(&listener, &count) {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            })
        };
        Ok(Self {
            _dir: dir,
            path,
            listener,
            count,
            stop,
            thread: Some(thread),
        })
    }

    /// The socket's path, for `SSH_AUTH_SOCK`.
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// The number of connections so far, including any still waiting to be accepted.
    pub fn connections(&self) -> usize {
        accept_pending(&self.listener, &self.count);
        self.count.load(Ordering::SeqCst)
    }
}

impl Drop for RecordingAgent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Accepts, counts and closes every waiting connection; whether there was one.
fn accept_pending(listener: &UnixListener, count: &AtomicUsize) -> bool {
    let mut any = false;
    while let Ok((stream, _)) = listener.accept() {
        count.fetch_add(1, Ordering::SeqCst);
        drop(stream);
        any = true;
    }
    any
}

/// A test SSH agent on a Unix socket (set its path as `SSH_AUTH_SOCK` for a child): it serves
/// the agent protocol's identities and sign requests from an in-memory [`SigningAgent`], one
/// request per connection, as rdy's caller agent sends them, and counts the requests it served.
/// Any other request, or a signer error, is answered with the bare agent failure.
///
/// [`SigningAgent`]: rdy_engine::signing_crypto::SigningAgent
pub struct SocketAgent {
    _dir: tempfile::TempDir,
    path: PathBuf,
    served: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The largest request packet the test agent reads.
const SOCKET_AGENT_PACKET_MAX: usize = 262_144;

impl SocketAgent {
    /// Binds a new socket in a fresh directory under [`short_tmp_root`] and serves `signer`.
    pub fn start(signer: Arc<dyn rdy_engine::signing_crypto::SigningAgent>) -> io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("rdy-sagent-")
            .tempdir_in(short_tmp_root())?;
        let path = dir.path().join("agent.sock");
        let listener = UnixListener::bind(&path)?;
        let served = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (served, stop) = (served.clone(), stop.clone());
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(mut conn) = conn else { continue };
                    if serve_agent_request(&mut conn, &*signer) {
                        served.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        };
        Ok(Self {
            _dir: dir,
            path,
            served,
            stop,
            thread: Some(thread),
        })
    }

    /// The socket's path, for `SSH_AUTH_SOCK`.
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// The requests answered so far (a failure reply counts too).
    pub fn requests(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }
}

impl Drop for SocketAgent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wake the blocked accept, which then sees the stop flag
        drop(std::os::unix::net::UnixStream::connect(&self.path));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Appends an SSH `string`: a big-endian length, then the bytes.
fn agent_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend(u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend(bytes);
}

/// Reads one SSH `string` at `*at` in `body`, every length checked against what remains.
fn agent_take<'a>(body: &'a [u8], at: &mut usize) -> Option<&'a [u8]> {
    let len_end = at.checked_add(4)?;
    let len = u32::from_be_bytes(body.get(*at..len_end)?.try_into().ok()?);
    let end = len_end.checked_add(usize::try_from(len).ok()?)?;
    let out = body.get(len_end..end)?;
    *at = end;
    Some(out)
}

/// Serves one request on `conn`; whether a reply was written.
fn serve_agent_request(
    conn: &mut std::os::unix::net::UnixStream,
    signer: &dyn rdy_engine::signing_crypto::SigningAgent,
) -> bool {
    use std::io::{Read, Write};
    let mut len = [0u8; 4];
    if conn.read_exact(&mut len).is_err() {
        return false;
    }
    let Ok(len) = usize::try_from(u32::from_be_bytes(len)) else {
        return false;
    };
    if len == 0 || len > SOCKET_AGENT_PACKET_MAX {
        return false;
    }
    let mut body = vec![0u8; len];
    if conn.read_exact(&mut body).is_err() {
        return false;
    }
    let failure = vec![5u8];
    let reply = match body[0] {
        11 => match signer.identities() {
            Ok(ids) => {
                let mut out = vec![12u8];
                out.extend(u32::try_from(ids.len()).unwrap_or(u32::MAX).to_be_bytes());
                for id in ids {
                    agent_string(&mut out, &id.public_key);
                    agent_string(&mut out, b"");
                }
                out
            }
            Err(_) => failure,
        },
        13 => {
            let mut at = 1;
            let key = agent_take(&body, &mut at);
            let data = agent_take(&body, &mut at);
            match (key, data) {
                (Some(key), Some(data)) => match signer.sign(key, data) {
                    Ok(sig) => {
                        let mut out = vec![14u8];
                        agent_string(&mut out, &sig);
                        out
                    }
                    Err(_) => failure,
                },
                _ => failure,
            }
        }
        _ => failure,
    };
    let mut packet = u32::try_from(reply.len())
        .unwrap_or(u32::MAX)
        .to_be_bytes()
        .to_vec();
    packet.extend(reply);
    conn.write_all(&packet).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    use std::time::{Duration, Instant};

    #[test]
    fn ps9_short_tmp_root_is_tmp() {
        assert_eq!(short_tmp_root(), Path::new("/tmp"));
    }

    #[test]
    fn ps9_a_child_runs_then_exits_after_kill_hard() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        own_process_group(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id();
        assert!(process_running(pid), "the child is running");
        assert!(!process_exited(pid), "the child has not exited");
        kill_hard(pid);
        let status = child.wait().expect("reap the child");
        assert_eq!(
            status.signal(),
            Some(9),
            "the child was killed by SIGKILL, not left to finish ({status:?})"
        );
        let until = Instant::now() + Duration::from_secs(10);
        while !process_exited(pid) {
            assert!(Instant::now() < until, "the child did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(process_exited(pid));
    }

    #[test]
    fn ps9_a_private_log_is_0600() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("log");
        let f = open_private_log(&path).expect("open the log");
        let mode = f.metadata().expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        open_private_log(&path).expect("open the log again (append)");
    }

    #[test]
    fn ps9_symlink_makes_a_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        std::fs::write(&target, b"x").expect("write");
        let link = dir.path().join("link");
        symlink(&target, &link).expect("symlink");
        assert_eq!(std::fs::read_link(&link).expect("read_link"), target);
        assert!(no_op_command().status().expect("run true").success());
    }

    #[test]
    fn ha1_a_shell_script_command_runs_its_script() {
        let out = shell_script("read l; printf '%s|%s' \"$l\" done")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().expect("stdin").write_all(b"line\n")?;
                c.wait_with_output()
            })
            .expect("run the script");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "line|done");
    }

    #[test]
    fn sig2_a_recording_agent_counts_each_connection() {
        let agent = RecordingAgent::start().expect("start the agent");
        assert_eq!(agent.connections(), 0, "no connection yet");
        drop(std::os::unix::net::UnixStream::connect(agent.socket_path()).expect("connect"));
        let mut held =
            std::os::unix::net::UnixStream::connect(agent.socket_path()).expect("connect");
        // a client that waits for a reply gets end of file, not a hang
        let mut buf = [0u8; 1];
        use std::io::Read;
        assert_eq!(held.read(&mut buf).expect("read"), 0);
        assert_eq!(agent.connections(), 2);
    }

    /// Makes a named pipe at `path`: a test's readiness channel from a child.
    fn fifo(path: &Path) {
        let made = Command::new("mkfifo")
            .arg(path)
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo {}", path.display());
    }

    /// Reads one line from the named pipe at `path` on a thread of its own; the line arrives on
    /// the returned channel. The open blocks until the child opens the pipe to write.
    fn fifo_line(path: PathBuf) -> std::sync::mpsc::Receiver<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let pipe = File::open(&path).expect("open the fifo");
            let mut line = String::new();
            std::io::BufReader::new(pipe)
                .read_line(&mut line)
                .expect("read the fifo");
            let _ = tx.send(line.trim_end().to_string());
        });
        rx
    }

    /// A hang guard: `f` on a thread, which must end within a minute (not a speed check).
    fn within_a_minute<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the bounded run did not end within 60 s")
    }

    /// Whether process group `pgid` still has a member that is not a zombie.
    fn group_has_live_member(pgid: u32) -> bool {
        let out = Command::new("ps")
            .args(["-A", "-o", "pgid=,stat="])
            .output()
            .expect("run ps");
        let want = pgid.to_string();
        String::from_utf8_lossy(&out.stdout).lines().any(|l| {
            let mut f = l.split_whitespace();
            f.next() == Some(want.as_str()) && f.next().is_some_and(|s| !s.starts_with('Z'))
        })
    }

    /// A hang guard for a process that a kill already ended: it may need a moment to leave the
    /// process table. Not a speed check.
    fn eventually(what: &str, done: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < until, "{what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn pre1_bounded_runner_caps_both_streams_and_reaps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ready = dir.path().join("ready");
        fifo(&ready);
        let descendant = fifo_line(ready.clone());
        // A descendant holds both pipes. The child names it on the readiness pipe, then writes
        // 600 bytes to each stream (each under the cap of 1000, both together over it) and
        // keeps running: only the cap can end it.
        let mut cmd = shell_script(
            "sleep 1000 & echo $! > \"$0\"; head -c 600 /dev/zero; head -c 600 /dev/zero >&2; \
             exec sleep 1000",
        );
        cmd.arg(&ready);
        let run = within_a_minute(move || run_bounded_inner(cmd, None, 1000, None, drop));
        let err = run.result.expect_err("1200 bytes pass a cap of 1000");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        assert_eq!(run.retained, 1000, "the bytes kept stop at the cap");
        assert_eq!(run.drains_joined, 2, "both drains were joined");
        let pid = run.pid.expect("the child started");
        assert!(process_exited(pid), "the direct child was reaped");
        let descendant: u32 = descendant
            .recv_timeout(Duration::from_secs(10))
            .expect("the child signalled readiness")
            .parse()
            .expect("a pid");
        eventually("the descendant exited", || process_exited(descendant));
        eventually("the group has no live member", || {
            !group_has_live_member(pid)
        });

        // A descendant that holds the pipes after its parent exits leaves no drain behind.
        let run = within_a_minute(|| {
            run_bounded_inner(shell_script("sleep 1000 & echo $!"), None, 1000, None, drop)
        });
        let out = run.result.expect("the child exits 0");
        assert!(out.status.success());
        assert_eq!(run.drains_joined, 2);
        let descendant: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("a pid");
        eventually("the descendant exited", || process_exited(descendant));
        let pid = run.pid.expect("the child started");
        eventually("the group has no live member", || {
            !group_has_live_member(pid)
        });

        // At the cap both streams are kept whole, and a nonzero status is an Output.
        let out = within_a_minute(|| {
            run_bounded(
                shell_script("printf out; printf err >&2; exit 3"),
                None,
                6,
                Duration::from_secs(600),
            )
        })
        .expect("6 bytes fit a cap of 6");
        assert_eq!(
            (out.stdout.as_slice(), out.stderr.as_slice()),
            (&b"out"[..], &b"err"[..])
        );
        assert_eq!(out.status.code(), Some(3));
        // One byte over, split across the streams.
        let err = within_a_minute(|| {
            run_bounded(
                shell_script("printf out; printf errs >&2"),
                None,
                6,
                Duration::from_secs(600),
            )
        })
        .expect_err("7 bytes pass a cap of 6");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");

        // stdin comes from the given file; a child that cannot start keeps its error kind.
        let input = dir.path().join("input");
        std::fs::write(&input, b"line\n").expect("write the input");
        let file = File::open(&input).expect("open the input");
        let out = within_a_minute(move || {
            run_bounded(
                shell_script("read l; printf '%s' \"$l\""),
                Some(file),
                100,
                Duration::from_secs(600),
            )
        })
        .expect("read stdin");
        assert_eq!(out.stdout, b"line");
        let err = run_bounded(
            Command::new("/nonexistent/rdy-qa-no-such-program"),
            None,
            100,
            Duration::from_secs(600),
        )
        .expect_err("no such program");
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
        assert_eq!(
            kill_process_group(0).expect_err("PID 0 is refused").kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn pre1_bounded_runner_timeout_cleans_up() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ready = dir.path().join("ready");
        fifo(&ready);
        // A child with a descendant signals readiness, then runs until it is stopped. The run
        // has no deadline of its own: only the injected expiry can end it.
        let mut cmd = shell_script("sleep 1000 & echo ready > \"$0\"; exec sleep 1000");
        cmd.arg(&ready);
        let (seen_tx, seen) = std::sync::mpsc::channel();
        let run = within_a_minute(move || {
            run_bounded_inner(cmd, None, 1 << 20, None, move |expiry| {
                std::thread::spawn(move || {
                    let line = fifo_line(ready)
                        .recv_timeout(Duration::from_secs(30))
                        .unwrap_or_default();
                    let _ = seen_tx.send(line);
                    expiry.expire();
                });
            })
        });
        assert_eq!(
            seen.recv_timeout(Duration::from_secs(10)).as_deref(),
            Ok("ready"),
            "the expiry came after the readiness signal"
        );
        let err = run.result.expect_err("the injected expiry ends the run");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert_eq!(run.drains_joined, 2, "both drains were joined");
        let pid = run.pid.expect("the child started");
        assert!(process_exited(pid), "the direct child was reaped");
        eventually("the group has no live member", || {
            !group_has_live_member(pid)
        });
    }

    #[test]
    fn pre1_native_release_facts_stay_in_the_platform_seam() {
        assert_eq!(
            release_target_of("macos", "aarch64", false).expect("macOS arm64"),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            release_target_of("linux", "x86_64", true).expect("Linux amd64"),
            "x86_64-unknown-linux-gnu"
        );
        for (os, arch, gnu) in [
            ("linux", "aarch64", true),
            ("macos", "x86_64", false),
            ("linux", "x86_64", false),
            ("windows", "x86_64", false),
        ] {
            let err = release_target_of(os, arch, gnu).expect_err("no release target");
            assert_eq!(err.kind(), io::ErrorKind::Unsupported, "{os}/{arch}");
        }
        match release_target() {
            Ok(t) => assert!(
                ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"].contains(&t),
                "{t}"
            ),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported, "{e}"),
        }

        assert_eq!(
            os_from_uname(b"Darwin 25.6.0 arm64\n").expect("one line"),
            "Darwin 25.6.0 arm64"
        );
        let longest = format!("{}\n", "a".repeat(4095));
        assert_eq!(
            os_from_uname(longest.as_bytes()).expect("4096 bytes"),
            "a".repeat(4095)
        );
        let too_long = format!("{}\n", "a".repeat(4096));
        for bad in [
            &b"Darwin \xff arm64\n"[..],
            b"Darwin 25.6.0 arm64",
            b"Darwin\n25.6.0\n",
            b"\n",
            b"",
            too_long.as_bytes(),
        ] {
            let err = os_from_uname(bad).expect_err("malformed probe output");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{bad:?}");
        }
        let os = release_os().expect("uname -srm");
        assert!(!os.is_empty() && !os.contains('\n'), "{os:?}");

        // The release module and its command keep no target conditional of their own.
        for (file, text) in [
            ("release_install.rs", include_str!("release_install.rs")),
            (
                "bin/qa-release-package.rs",
                include_str!("bin/qa-release-package.rs"),
            ),
        ] {
            for token in [
                "cfg(target",
                "target_os",
                "target_arch",
                "target_env",
                "env::consts",
            ] {
                assert!(!text.contains(token), "{file} holds {token}");
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let f = dir.path().join("f");
        std::fs::write(&f, b"x").expect("write");
        set_mode(&f, 0o4751).expect("set the mode");
        assert_eq!(mode_of(&f).expect("read the mode"), 0o4751);
        set_mode(&f, 0o644).expect("set the mode");
        assert_eq!(mode_of(&f).expect("read the mode"), 0o644);
    }
}
