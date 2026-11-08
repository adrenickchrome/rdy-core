//! Starting a detached daemon: the `rdy serve --detach` command a client spawns, the child's own
//! `detach`, and the private pipe that carries a start's one-use token to the launched process
//! (`rdy_store::start_lock`).

use crate::layout::RunDir;
use crate::lock::open_no_follow;
use crate::socket::check_socket_path;
use crate::sys;
use rdy_store::start_lock::HandoffToken;
use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

/// What a start has armed for this thread while its spawner runs: the pipe's read end, until
/// `serve_command` takes it for the child's stdin, then the pid `spawn_serve` launched with it.
struct Armed {
    reader: Option<PipeReader>,
    pid: Option<u32>,
}

thread_local! {
    static ARMED: RefCell<Option<Armed>> = const { RefCell::new(None) };
}

/// A start's preparation for one launch: the spawner runs between [`Launch::arm`] and
/// [`Launch::finish`], on this thread, with no await between them.
pub(crate) struct Launch {
    writer: PipeWriter,
}

/// What a spawner did with an armed [`Launch`].
pub(crate) enum Launched {
    /// It starts its daemon its own way, with no `serve_command` (a test's in-process or echo
    /// daemon). The starter keeps the start lock until that daemon answers.
    OwnWay,
    /// It built the command and launched it some other way: no process to tie a reservation to.
    /// The child reads no token and takes its own guard, so the starter lets go.
    NoProcess,
    /// `serve_command` and `spawn_serve` launched this process.
    Child(LaunchedChild),
}

/// A launched process that waits for its token.
pub(crate) struct LaunchedChild {
    pub(crate) pid: u32,
    pub(crate) token: HandoffToken,
    writer: PipeWriter,
}

impl LaunchedChild {
    /// Sends the token and closes the pipe. A child that is gone reads nothing, and the starter
    /// sees its exit: a failed write changes nothing.
    pub(crate) fn send_token(mut self) {
        let _ = self.writer.write_all(self.token.text());
    }
}

impl Launch {
    /// Arms this thread for one launch.
    pub(crate) fn arm() -> io::Result<Launch> {
        let (reader, writer) = io::pipe()?;
        ARMED.with(|a| {
            *a.borrow_mut() = Some(Armed {
                reader: Some(reader),
                pid: None,
            });
        });
        Ok(Launch { writer })
    }

    /// Disarms this thread and says what the spawner did.
    pub(crate) fn finish(self) -> Launched {
        let armed = ARMED.with(|a| a.borrow_mut().take());
        match armed {
            None
            | Some(Armed {
                reader: Some(_), ..
            }) => Launched::OwnWay,
            Some(Armed { pid: None, .. }) => Launched::NoProcess,
            Some(Armed { pid: Some(pid), .. }) => Launched::Child(LaunchedChild {
                pid,
                token: HandoffToken::new(),
                writer: self.writer,
            }),
        }
    }
}

/// The token an armed start sends on this process's stdin, waiting until `until`. `None` when the
/// stdin ends first (`/dev/null`: no start armed this launch, or its starter died), or holds no
/// token. A token alone gives no right: `rdy_store::start_lock` checks it against the reservation,
/// with this process's id and the project.
pub(crate) fn inherited_token(until: Instant) -> Option<HandoffToken> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rdy-start-token".into())
        .spawn(move || {
            let mut text = [0u8; HandoffToken::TEXT_LEN];
            let read = io::stdin().lock().read_exact(&mut text);
            let _ = tx.send(read.ok().map(|()| text));
        })
        .ok()?;
    let left = until.saturating_duration_since(Instant::now());
    let text = rx.recv_timeout(left).ok().flatten()?;
    HandoffToken::from_text(&text)
}

/// The stdin of a command `serve_command` builds: an armed start's pipe, else `/dev/null`.
fn serve_stdin() -> Stdio {
    let reader = ARMED.with(|a| a.borrow_mut().as_mut().and_then(|a| a.reader.take()));
    reader.map_or_else(Stdio::null, Stdio::from)
}

/// The command that starts a detached daemon for `project`, unspawned: `<rdy_bin> serve --detach
/// --spawned --project <project>` in `sys::daemon_start_dir` (`--spawned` tells `rdy serve --detach`
/// to serve in that process rather than spawn another), stdin from /dev/null (or, under a start armed on
/// this thread, the private pipe of its token), stdout and stderr appended to the run dir's `daemon.log` (created 0600; refused if it is a symlink), every `RDY_*`
/// variable removed from its environment, and a new session (so a new process group) of its own
/// from before its exec. The run dir is made absolute first, and a socket path the daemon could
/// not bind (`socket::check_socket_path`) is refused on that absolute path, before anything is
/// created, rather than spawning a daemon that would only die at its bind. A relative `project`
/// is made absolute against the caller's working directory too: where the child runs, the relative
/// path would name something else, such as `<project>/<project>`.
pub fn serve_command(rdy_bin: &Path, project: &Path, run: &RunDir) -> io::Result<Command> {
    let run = run.absolute()?;
    check_socket_path(&run)?;
    let project = std::path::absolute(project)?;
    run.ensure()?;
    let log = open_no_follow(
        sys::private_file(OpenOptions::new().append(true).create(true)),
        &run.log_file(),
    )?;
    // a file left by an older run keeps its mode through open; set it exactly
    sys::make_private_file(&log)?;
    let (out, err) = (log.try_clone()?, log.try_clone()?);
    let mut cmd = Command::new(rdy_bin);
    cmd.arg("serve")
        .arg("--detach")
        .arg("--spawned")
        .arg("--project")
        .arg(&project)
        .current_dir(sys::daemon_start_dir(rdy_bin, &project))
        .stdin(serve_stdin())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    // a new session, so the daemon's own `detach` can leave ours
    sys::new_session_on_spawn(&mut cmd);
    for (k, _) in std::env::vars_os() {
        if k.as_encoded_bytes().starts_with(b"RDY_") {
            cmd.env_remove(k);
        }
    }
    Ok(cmd)
}

/// A spawned daemon, as its spawner watches it.
pub struct Spawned {
    /// The child's exit code once it has exited (`128 + n` when signal `n` ended it, as a shell
    /// reports it), `None` while it runs.
    pub exited: Box<dyn FnMut() -> Option<i32> + Send>,
}

/// Spawns `cmd` and returns without waiting for it to finish. A reaper thread waits on the child
/// and stores its exit code, which `exited` reads; so the child is reaped when it exits whether
/// or not the `Spawned` is still held. `setsid` does not reparent the child: without the reaper a
/// long-lived spawner (`rdy edit`, an `rdy mcp` proxy) would keep an exited daemon as a zombie,
/// which `process_alive` still reports.
pub fn spawn_serve(cmd: &mut Command) -> io::Result<Spawned> {
    let mut child = cmd.spawn()?;
    // a command `serve_command` armed: this is the process the start's token goes to
    ARMED.with(|a| {
        if let Some(a) = a.borrow_mut().as_mut()
            && a.reader.is_none()
        {
            a.pid = Some(child.id());
        }
    });
    let code = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&code);
    std::thread::Builder::new()
        .name("rdy-daemon-reaper".into())
        .spawn(move || {
            // an error here means the child cannot be waited on; it then never reports an exit,
            // as with a child that runs
            if let Ok(status) = child.wait() {
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(sys::exit_code(status));
            }
        })?;
    Ok(Spawned {
        exited: Box::new(move || *code.lock().unwrap_or_else(|e| e.into_inner())),
    })
}

/// Whether a process `pid` exists (a process of another account counts).
pub fn process_alive(pid: u32) -> bool {
    sys::alive(pid)
}

/// Sends `SIGTERM` to `pid`, which `rdy serve` takes as `rdy serve --stop`. `Ok(true)` when it
/// was sent, `Ok(false)` when the process is gone (`ESRCH`). A `pid` of 1 or less is refused:
/// never init, and never a process group (`kill(0, ..)` and negative pids). Unix only: no process
/// is ended by its pid alone on Windows (`sys::stop_refused_daemon`).
#[cfg(unix)]
pub fn terminate(pid: u32) -> io::Result<bool> {
    sys::terminate(pid)
}

/// Leaves the caller's session (`setsid`) or console. A caller that has left already (a child of
/// `serve_command`) is `Ok`. Called by `rdy serve --detach` after its argument parsing and before
/// it opens the store.
pub fn detach() -> io::Result<()> {
    sys::detach()
}

/// The caller's process group id (`getpgid(0)`), for the tests.
#[cfg(unix)]
#[doc(hidden)]
pub fn own_pgid() -> i32 {
    sys::own_pgid()
}

/// The caller's session id (`getsid(0)`), for the tests.
#[cfg(unix)]
#[doc(hidden)]
pub fn own_sid() -> i32 {
    sys::own_sid()
}
