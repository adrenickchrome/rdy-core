//! The harness's Unix-only helpers, in one place (FW-XP1 D-1RXEA).
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
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

/// The directory that holds `path` and `path`'s own entry name in it.
fn entry_of(path: &Path) -> io::Result<(&Path, &std::ffi::OsStr)> {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => Ok((parent, name)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} names no directory entry", path.display()),
        )),
    }
}

/// Opens the directory `path` for the `*_at` calls below; links in `path` are followed (the
/// caller trusts this path). Anything that is not a directory is refused.
pub fn open_dir(path: &Path) -> io::Result<File> {
    rdy_store::pio::open_dir(path)
}

/// Creates the new directory `name` in the open directory `parent`, private to this user
/// whatever the umask, and returns a handle to it. mkdir makes it with mode 0o700, which a
/// umask can only narrow; it is then set to exactly 0o700 through a handle that never follows
/// a link, and checked to be a directory this user owns. An existing `name` is
/// `ErrorKind::AlreadyExists`. If a step after mkdir fails, the directory is removed again
/// while it is still empty: by name when it could not be opened (rmdir refuses a link and a
/// directory that is not empty), else only while `name` still holds the directory this user
/// made.
pub fn create_private_dir_at(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    create_private_dir_with(parent, name, &open_made_dir)
}

/// How [`create_private_dir_at`] opens the directory it just made; tests make it fail.
type OpenMadeDir = dyn Fn(&File, &std::ffi::OsStr) -> io::Result<File>;

/// Opens the directory `name` that mkdir just made in `parent`, never through a link.
fn open_made_dir(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    use rdy_store::pio;
    match pio::open_dir_at(parent, name) {
        // a umask that took the owner's bits: give them back, then open
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            pio::set_mode_at(parent, name, 0o700).and_then(|()| pio::open_dir_at(parent, name))
        }
        opened => opened,
    }
}

/// [`create_private_dir_at`], opening the new directory with `open`.
fn create_private_dir_with(
    parent: &File,
    name: &std::ffi::OsStr,
    open: &OpenMadeDir,
) -> io::Result<File> {
    use rdy_store::pio;
    pio::create_private_dir_at(parent, name)?;
    let dir = match open(parent, name) {
        Ok(dir) => dir,
        Err(e) => {
            // no handle yet: rmdir by name removes only an empty directory, never a link
            let _ = pio::remove_dir_at(parent, name);
            return Err(e);
        }
    };
    let checked = (|| {
        dir.set_permissions(pio::permissions(0o700))?;
        let meta = dir.metadata()?;
        if meta.is_dir() && pio::owner(&meta) == pio::own_uid() && pio::is_private_dir(&meta) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "not a private directory of this user",
            ))
        }
    })();
    match checked {
        Ok(()) => Ok(dir),
        Err(e) => {
            let ours = dir
                .metadata()
                .is_ok_and(|m| pio::owner(&m) == pio::own_uid());
            if ours && holds_file(parent, name, &dir).unwrap_or(false) {
                let _ = pio::remove_dir_at(parent, name);
            }
            Err(e)
        }
    }
}

/// Opens the file `path` for reading, never through a link at its last entry (the directories
/// above it are trusted) and without waiting on a FIFO. A link there is
/// `ErrorKind::InvalidInput`, and so is a file that is not regular and cannot be opened (a
/// socket, a FIFO or device without access): which kind of failure the open had depends on
/// the kind of file. The caller judges the file's kind and size from the handle and reads that
/// same handle, so a path changed after the open changes nothing.
pub fn open_input(path: &Path) -> io::Result<File> {
    let (parent, name) = entry_of(path)?;
    let dir = rdy_store::pio::open_dir(parent)?;
    rdy_store::pio::open_read_at(&dir, name).map_err(|e| {
        if rdy_store::pio::is_link_refusal(&e) {
            return io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a symbolic link", path.display()),
            );
        }
        // the open failed, so only the error is at stake: the entry's own type, unfollowed
        match std::fs::symlink_metadata(path) {
            Ok(meta) if !meta.file_type().is_file() => io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ),
            _ => e,
        }
    })
}

/// Creates the new file `name` in the open directory `dir`, for reading and writing, never
/// through a link: anything already at `name` is `ErrorKind::AlreadyExists`. Its mode is 0o600
/// less the umask until [`set_file_mode`] sets it.
pub fn create_new_at(dir: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    rdy_store::pio::create_private_at(dir, name)
}

/// Whether the entry `name` in the open directory `dir` is still the file or directory `file`
/// (the same device and inode). A missing entry, or a link in its place, is not.
pub fn holds_file(dir: &File, name: &std::ffi::OsStr, file: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    match rdy_store::pio::open_read_at(dir, name) {
        Ok(found) => {
            let (a, b) = (found.metadata()?, file.metadata()?);
            Ok(a.dev() == b.dev() && a.ino() == b.ino())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound || rdy_store::pio::is_link_refusal(&e) => {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Whether `path`, its links followed, opens the open directory `dir` now (the same device and
/// inode). A path that cannot be opened as a directory does not.
pub fn opens_dir(path: &Path, dir: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (open_dir(path).and_then(|d| d.metadata()), dir.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Whether the open directory `dir` holds no entry (`.` and `..` aside). It is listed in this
/// process through the handle, never through a path, and starts no process.
pub fn is_empty_dir(dir: &File) -> io::Result<bool> {
    rdy_store::pio::is_empty_dir(dir)
}

/// Removes the entry `name` (not a directory) from the open directory `dir`; a link is removed,
/// never followed.
pub fn remove_file_at(dir: &File, name: &std::ffi::OsStr) -> io::Result<()> {
    rdy_store::pio::remove_file_at(dir, name)
}

/// Removes the empty directory `name` from the open directory `dir`; a link is never followed.
pub fn remove_dir_at(dir: &File, name: &std::ffi::OsStr) -> io::Result<()> {
    rdy_store::pio::remove_dir_at(dir, name)
}

/// Renames the entry `from` to `to`, both in the open directory `dir`.
pub fn rename_at(dir: &File, from: &std::ffi::OsStr, to: &std::ffi::OsStr) -> io::Result<()> {
    rdy_store::pio::rename_at(dir, from, to)
}

/// Makes the child that `cmd` spawns start in the open directory `dir`, whatever later happens
/// to the paths that led to it; a program named `./name` is found in `dir`.
pub fn start_in_dir(cmd: &mut Command, dir: &File) -> io::Result<()> {
    rdy_store::pio::start_in_dir(cmd, dir)
}

/// Sets the permission bits of the open file `file` to `mode`.
pub fn set_file_mode(file: &File, mode: u32) -> io::Result<()> {
    file.set_permissions(rdy_store::pio::permissions(mode))
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
/// and stdout and stderr captured under one combined cap of `max_output` bytes.
///
/// When the direct child exits, the group is stopped, so a descendant in the group that still
/// holds a stream cannot keep a drain running. On overflow, at `deadline`, on a read error or a
/// setup failure, the group is stopped too; the runner then waits for the direct child, cancels
/// both drains and joins them. A drain's read waits at most [`DRAIN_TICK`] before it checks for
/// cancellation, so a process that left the group and keeps the streams open cannot hold the
/// runner past its deadline. Overflow is `InvalidData`, the deadline `TimedOut`, and a setup,
/// read or cleanup failure keeps its own kind. A cleanup failure is the result even after an
/// earlier failure, and never becomes success. A nonzero exit status is an `Output`, for the
/// caller to judge.
///
/// The streams are Unix socket pairs rather than pipes: std gives a socket a read timeout,
/// which is what makes a drain cancellable.
pub fn run_bounded(
    command: Command,
    stdin: Option<File>,
    max_output: usize,
    deadline: Duration,
) -> io::Result<Output> {
    let until = Instant::now().checked_add(deadline);
    run_bounded_inner(command, stdin, None, max_output, until, drop).result
}

/// [`run_bounded`] with the child's stdout written to the open file `stdout` instead of
/// captured: only stderr counts against `max_output`, and the `Output`'s stdout is empty. The
/// child writes through the handle, never through a path.
pub fn run_bounded_to(
    command: Command,
    stdin: Option<File>,
    stdout: File,
    max_output: usize,
    deadline: Duration,
) -> io::Result<Output> {
    let until = Instant::now().checked_add(deadline);
    run_bounded_inner(command, stdin, Some(stdout), max_output, until, drop).result
}

#[cfg(test)]
thread_local! {
    /// How many processes the bounded runner has started on this thread (a test hook).
    static RUNNER_STARTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many processes the bounded runner has started on this thread so far: tests use it to
/// see every process a check starts, not only those a hook is told of.
#[cfg(test)]
pub(crate) fn runner_starts() -> usize {
    RUNNER_STARTS.with(std::cell::Cell::get)
}

/// How long a drain's read waits before it checks whether the run cancelled it.
const DRAIN_TICK: Duration = Duration::from_millis(20);

/// One captured stream: the runner's end, and the child's end for `Stdio`.
fn stream_pair() -> io::Result<(UnixStream, Stdio)> {
    let (ours, theirs) = UnixStream::pair()?;
    ours.set_read_timeout(Some(DRAIN_TICK))?;
    Ok((ours, Stdio::from(std::os::fd::OwnedFd::from(theirs))))
}

/// [`run_bounded`] with stdout to a file when `stdout` is given (as [`run_bounded_to`]), an
/// absolute deadline (`None`: none) and `on_start`, which receives the run's [`Expiry`] once
/// the child and its workers started.
pub(crate) fn run_bounded_inner(
    mut command: Command,
    stdin: Option<File>,
    stdout: Option<File>,
    max_output: usize,
    deadline: Option<Instant>,
    on_start: impl FnOnce(Expiry),
) -> BoundedRun {
    let not_started = |e| BoundedRun {
        result: Err(e),
        retained: 0,
        drains_joined: 0,
        pid: None,
    };
    let captured_out = match stdout {
        Some(file) => Ok((None, Stdio::from(file))),
        None => stream_pair().map(|(ours, theirs)| (Some(ours), theirs)),
    };
    let (out, err) = match (captured_out, stream_pair()) {
        (Ok(out), Ok(err)) => (out, err),
        (Err(e), _) | (_, Err(e)) => return not_started(e),
    };
    own_process_group(&mut command);
    command
        .stdin(stdin.map_or_else(Stdio::null, Stdio::from))
        .stdout(out.1)
        .stderr(err.1);
    #[cfg(test)]
    RUNNER_STARTS.with(|n| n.set(n.get() + 1));
    let spawned = command.spawn();
    // the command holds the child's ends: drop them, or no drain would see the end of a stream
    drop(command);
    let child = match spawned {
        Ok(child) => child,
        Err(e) => return not_started(e),
    };
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    let count = Arc::new(AtomicUsize::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let mut failure: Option<io::Error> = None;
    // one drain per captured stream; a stdout that goes to a file has none
    let mut drains: [Option<std::thread::JoinHandle<Vec<u8>>>; 2] = [None, None];
    for (slot, stream) in drains.iter_mut().zip([out.0, Some(err.0)]) {
        let Some(stream) = stream else { continue };
        match spawn_drain(stream, &count, &cancel, max_output, &tx) {
            Ok(handle) => *slot = Some(handle),
            Err(e) => failure = failure.or(Some(e)),
        }
    }
    let running = drains.iter().flatten().count();
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
                        // a descendant may still hold a stream: stop the whole group
                        Ok(s) => match kill_process_group(pid) {
                            Ok(()) => status = Some(s),
                            Err(e) => break Some(e),
                        },
                        Err(e) => break Some(e),
                    }
                }
                RunEvent::DrainDone => drains_done += 1,
            }
            if status.is_some() && drains_done == running {
                break None;
            }
        };
    }

    // Cleanup: stop the group, reap the direct child, then cancel and join the drains, whose
    // reads end within a tick whoever still holds the streams.
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
    cancel.store(true, Ordering::SeqCst);
    if let Some(waiter) = waiter
        && waiter.join().is_err()
    {
        cleanup = cleanup.or(Some(lost()));
    }
    let mut drains_joined = 0;
    let [stdout, stderr] = drains.map(|drain| match drain.map(|d| d.join()) {
        Some(Ok(bytes)) => {
            drains_joined += 1;
            bytes
        }
        Some(Err(_)) => {
            cleanup = cleanup.take().or(Some(lost()));
            Vec::new()
        }
        None => Vec::new(),
    });
    let retained = stdout.len() + stderr.len();
    let result = settle(failure, cleanup, status, stdout, stderr);
    BoundedRun {
        result,
        retained,
        drains_joined,
        pid: Some(pid),
    }
}

/// The runner's own failure: a worker it waits on is gone.
fn lost() -> io::Error {
    io::Error::other("the bounded runner lost its workers")
}

/// A bounded run's result from what its run and its cleanup found: a cleanup failure first,
/// then the run's own failure, then the child's output.
fn settle(
    failure: Option<io::Error>,
    cleanup: Option<io::Error>,
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> io::Result<Output> {
    match (cleanup.or(failure), status) {
        (Some(e), _) => Err(e),
        (None, Some(status)) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        (None, None) => Err(lost()),
    }
}

/// Starts a drain of `stream`.
fn spawn_drain<R: Read + Send + 'static>(
    stream: R,
    count: &Arc<AtomicUsize>,
    cancel: &Arc<AtomicBool>,
    max_output: usize,
    tx: &mpsc::Sender<RunEvent>,
) -> io::Result<std::thread::JoinHandle<Vec<u8>>> {
    let (count, cancel, tx) = (count.clone(), cancel.clone(), tx.clone());
    std::thread::Builder::new()
        .name("rdy-qa-drain".into())
        .spawn(move || drain_bounded(stream, &count, &cancel, max_output, &tx))
}

/// Reads `stream` to its end, keeping each byte only while the count both streams share stays
/// within `max_output`; a byte is counted before it is kept. A read that times out only checks
/// `cancel`, which stops the drain. Reports overflow or a read error, then stops; always
/// reports that it is done.
fn drain_bounded(
    mut stream: impl Read,
    count: &AtomicUsize,
    cancel: &AtomicBool,
    max_output: usize,
    tx: &mpsc::Sender<RunEvent>,
) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    while !cancel.load(Ordering::SeqCst) {
        match stream.read(&mut buf) {
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
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) => {}
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

/// Test helpers that the release module's tests share: named pipes for readiness, and hang
/// guards (never speed checks).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Makes a named pipe at `path`: a test's readiness channel from a child.
    pub(crate) fn fifo(path: &Path) {
        let made = Command::new("mkfifo")
            .arg(path)
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo {}", path.display());
    }

    /// Reads one line from the named pipe at `path` on a thread of its own; the line arrives on
    /// the returned channel. The open blocks until the child opens the pipe to write.
    pub(crate) fn fifo_line(path: PathBuf) -> std::sync::mpsc::Receiver<String> {
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

    /// Binds a Unix socket at `path`: a file that is not regular and that no `open` can open.
    /// The socket file stays at `path` after the listener is dropped.
    pub(crate) fn unix_socket(path: &Path) -> std::os::unix::net::UnixListener {
        std::os::unix::net::UnixListener::bind(path).expect("bind a unix socket")
    }

    /// A hang guard: `f` on a thread, which must end within a minute (not a speed check).
    pub(crate) fn within_a_minute<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the bounded run did not end within 60 s")
    }

    /// A hang guard for a process that a kill already ended: it may need a moment to leave the
    /// process table. Not a speed check.
    pub(crate) fn eventually(what: &str, done: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < until, "{what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
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
    fn pre3_is_empty_dir_lists_the_open_directory_in_process() {
        let root = tempfile::tempdir().expect("tempdir");
        let a = root.path().join("a");
        std::fs::create_dir(&a).expect("mkdir");
        let dir = open_dir(&a).expect("open");
        let starts = runner_starts();
        assert!(
            is_empty_dir(&dir).expect("list"),
            "a new directory is empty"
        );
        assert!(is_empty_dir(&dir).expect("list"), "the same answer again");
        // the open directory is listed, not its old path: moved, with a full directory in its
        // place, it is still empty
        let b = root.path().join("b");
        std::fs::rename(&a, &b).expect("rename");
        std::fs::create_dir(&a).expect("mkdir");
        std::fs::write(a.join("theirs"), b"x").expect("write");
        assert!(is_empty_dir(&dir).expect("list"), "moved, still empty");
        // a hidden file or a directory in it is an entry
        std::fs::write(b.join(".hidden"), b"x").expect("write");
        assert!(!is_empty_dir(&dir).expect("list"), "a hidden file");
        std::fs::remove_file(b.join(".hidden")).expect("rm");
        std::fs::create_dir(b.join("sub")).expect("mkdir");
        assert!(!is_empty_dir(&dir).expect("list"), "a directory");
        assert_eq!(runner_starts(), starts, "no process was started");
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

    #[test]
    fn pre1_private_dir_is_removed_when_it_cannot_be_opened() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("keep"), b"sentinel\n").expect("sentinel");
        let parent = open_dir(tmp.path()).expect("open the parent");
        let out = tmp.path().join("out");
        let refused: &OpenMadeDir = &|_, _| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        // the umask path: the mode is given back, then the reopen fails
        let reopen_fails: &OpenMadeDir = &|p, n| {
            rdy_store::pio::set_mode_at(p, n, 0o700)?;
            Err(io::Error::other("the reopen failed"))
        };
        for (what, open) in [("open", refused), ("set mode, then reopen", reopen_fails)] {
            let got = create_private_dir_with(&parent, "out".as_ref(), open);
            assert!(got.is_err(), "{what}: the failure is the result");
            assert_eq!(
                std::fs::symlink_metadata(&out).map_err(|e| e.kind()).err(),
                Some(io::ErrorKind::NotFound),
                "{what}: the new OUT is gone"
            );
            assert_eq!(
                std::fs::read(tmp.path().join("keep")).expect("keep"),
                b"sentinel\n"
            );
            // and the next run can make OUT again
            create_private_dir_at(&parent, "out".as_ref()).expect("make OUT again");
            std::fs::remove_dir(&out).expect("rmdir OUT");
        }
        // a directory that was already there is AlreadyExists and stays
        std::fs::create_dir(&out).expect("occupy OUT");
        let got = create_private_dir_with(&parent, "out".as_ref(), refused);
        assert_eq!(
            got.err().map(|e| e.kind()),
            Some(io::ErrorKind::AlreadyExists)
        );
        assert!(out.is_dir(), "an existing OUT is never removed");
    }

    #[test]
    fn pre1_open_input_calls_an_unopenable_nonregular_file_invalid_input() {
        let tmp = tempfile::tempdir_in(short_tmp_root()).expect("tempdir");
        let sock = tmp.path().join("sock");
        let _listener = unix_socket(&sock);
        let fifo_path = tmp.path().join("fifo");
        fifo(&fifo_path);
        set_mode(&fifo_path, 0o000).expect("chmod the fifo");
        for (what, path) in [
            ("a socket", &sock),
            ("a fifo without read access", &fifo_path),
        ] {
            let got = open_input(path);
            assert_eq!(
                got.as_ref().err().map(io::Error::kind),
                Some(io::ErrorKind::InvalidInput),
                "{what}: {got:?}"
            );
        }
        // an unreadable regular file is a file operation failure, not an invalid input
        if rdy_store::pio::own_uid() != 0 {
            let locked = tmp.path().join("locked");
            std::fs::write(&locked, b"x").expect("write");
            set_mode(&locked, 0o000).expect("chmod");
            assert_eq!(
                open_input(&locked).err().map(|e| e.kind()),
                Some(io::ErrorKind::PermissionDenied)
            );
        }
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
        let run = within_a_minute(move || run_bounded_inner(cmd, None, None, 1000, None, drop));
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
            run_bounded_inner(
                shell_script("sleep 1000 & echo $!"),
                None,
                None,
                1000,
                None,
                drop,
            )
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
    fn pre1_bounded_runner_writes_stdout_through_a_handle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("archive");
        let file = File::create_new(&path).expect("create");
        // The name is taken over before the child runs: its stdout still reaches the file the
        // handle opened, and the newcomer keeps its bytes.
        std::fs::rename(&path, dir.path().join("moved")).expect("move");
        std::fs::write(&path, b"newcomer\n").expect("newcomer");
        let out = within_a_minute(move || {
            run_bounded_to(
                shell_script("printf 'archive bytes over the cap'; printf err >&2"),
                None,
                file,
                3,
                Duration::from_secs(600),
            )
        })
        .expect("stderr fits a cap of 3");
        assert!(out.status.success());
        assert!(out.stdout.is_empty(), "stdout went to the file");
        assert_eq!(out.stderr, b"err");
        assert_eq!(
            std::fs::read(dir.path().join("moved")).expect("read"),
            b"archive bytes over the cap"
        );
        assert_eq!(std::fs::read(&path).expect("read"), b"newcomer\n");

        // stderr alone counts against the cap, and only its drain runs.
        let file = File::create_new(dir.path().join("second")).expect("create");
        let run = within_a_minute(move || {
            run_bounded_inner(
                shell_script("printf errs >&2"),
                None,
                Some(file),
                3,
                None,
                drop,
            )
        });
        let err = run.result.expect_err("4 bytes pass a cap of 3");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        assert_eq!(run.drains_joined, 1, "one drain, for stderr");
        assert_eq!(run.retained, 3);
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
            run_bounded_inner(cmd, None, None, 1 << 20, None, move |expiry| {
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

    /// SIGKILLs process `pid` when dropped: a test's escaped process never outlives the test.
    struct KillOnDrop(u32);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = Command::new("kill")
                .args(["-9", &self.0.to_string()])
                .stderr(Stdio::null())
                .status();
        }
    }

    #[test]
    fn pre1_bounded_runner_timeout_cleans_up_after_an_escaped_pipe_holder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (ready, go) = (dir.path().join("ready"), dir.path().join("go"));
        fifo(&ready);
        fifo(&go);
        // A descendant leaves the child's process group, keeps stdout and stderr, and names
        // itself and the child on the readiness pipe. The child then waits for the go signal
        // and exits, so only the escaped process holds the pipes. The run has no deadline of
        // its own: only the injected expiry can end it.
        let mut cmd = shell_script(
            "perl -e 'setpgrp(0, 0) or die; open(my $f, \">\", $ARGV[0]) or die; \
             print $f \"$$ $ARGV[1]\\n\"; close $f; sleep 1000' \"$0\" $$ & \
             read go < \"$1\"; exit 0",
        );
        cmd.arg(&ready).arg(&go);
        let (seen_tx, seen) = std::sync::mpsc::channel();
        let run = within_a_minute(move || {
            run_bounded_inner(cmd, None, None, 1 << 20, None, move |expiry| {
                std::thread::spawn(move || {
                    let line = fifo_line(ready)
                        .recv_timeout(Duration::from_secs(30))
                        .unwrap_or_default();
                    let pids: Vec<u32> = line
                        .split_whitespace()
                        .filter_map(|p| p.parse().ok())
                        .collect();
                    let _ = seen_tx.send(pids.clone());
                    if let [_, child] = pids[..] {
                        use std::io::Write;
                        let _ = File::options()
                            .write(true)
                            .open(&go)
                            .and_then(|mut g| g.write_all(b"go\n"));
                        eventually("the direct child exited", || process_exited(child));
                    }
                    expiry.expire();
                });
            })
        });
        let pids = seen
            .recv_timeout(Duration::from_secs(10))
            .expect("the child signalled readiness");
        let [escaped, child] = pids[..] else {
            panic!("the readiness line named {pids:?}");
        };
        let _escaped = KillOnDrop(escaped);
        let err = run.result.expect_err("the injected expiry ends the run");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert_eq!(run.drains_joined, 2, "both drains were joined");
        assert_eq!(run.pid, Some(child));
        assert!(process_exited(child), "the direct child was reaped");
        assert!(
            process_running(escaped),
            "the run ended while the escaped process still holds the pipes"
        );
    }

    #[test]
    fn pre1_bounded_runner_cleanup_failure_overrides() {
        let status = Command::new("true").status().expect("run true");
        let err = |kind| Some(io::Error::new(kind, "x"));
        let kind = |r: io::Result<Output>| r.expect_err("a failed run").kind();
        // A cleanup failure is the result, whatever the run found before it, and keeps its kind.
        for failure in [
            None,
            err(io::ErrorKind::InvalidData),
            err(io::ErrorKind::TimedOut),
        ] {
            assert_eq!(
                kind(settle(
                    failure,
                    err(io::ErrorKind::PermissionDenied),
                    Some(status),
                    vec![],
                    vec![]
                )),
                io::ErrorKind::PermissionDenied
            );
        }
        assert_eq!(
            kind(settle(
                err(io::ErrorKind::TimedOut),
                None,
                None,
                vec![],
                vec![]
            )),
            io::ErrorKind::TimedOut
        );
        let out = settle(None, None, Some(status), b"o".to_vec(), b"e".to_vec()).expect("a run");
        assert_eq!((out.stdout, out.stderr), (b"o".to_vec(), b"e".to_vec()));
        assert!(settle(None, None, None, vec![], vec![]).is_err());
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
        // The release files name no design block.
        let block = regex::Regex::new(r"\b(D|tb|FW)-[0-9A-Z]{2,}").expect("regex");
        for (file, text) in [
            ("release_install.rs", include_str!("release_install.rs")),
            (
                "bin/qa-release-package.rs",
                include_str!("bin/qa-release-package.rs"),
            ),
            (
                "tests/release/release_package.rs",
                include_str!("../tests/release/release_package.rs"),
            ),
            (
                "tests/release/main.rs",
                include_str!("../tests/release/main.rs"),
            ),
        ] {
            let found: Vec<&str> = block.find_iter(text).map(|m| m.as_str()).collect();
            assert!(found.is_empty(), "{file} names {found:?}");
        }
        // Nor does the runner's own text in this file.
        let runner = include_str!("platform.rs");
        let runner = &runner[runner
            .find("pub const RELEASE_CHILD_OUTPUT_MAX")
            .expect("runner")
            ..runner.find("pub fn shell_script").expect("runner end")];
        let found: Vec<&str> = block.find_iter(runner).map(|m| m.as_str()).collect();
        assert!(found.is_empty(), "the runner names {found:?}");

        let dir = tempfile::tempdir().expect("tempdir");
        let f = dir.path().join("f");
        std::fs::write(&f, b"x").expect("write");
        set_mode(&f, 0o4751).expect("set the mode");
        assert_eq!(mode_of(&f).expect("read the mode"), 0o4751);
        set_mode(&f, 0o644).expect("set the mode");
        assert_eq!(mode_of(&f).expect("read the mode"), 0o644);
    }
}
