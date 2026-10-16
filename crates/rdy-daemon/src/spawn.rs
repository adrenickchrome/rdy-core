//! Starting a detached daemon: the `rdy serve --detach` command a client spawns, and the
//! child's own `detach`.

use crate::layout::RunDir;
use crate::lock::open_no_follow;
use crate::socket::check_socket_path;
use crate::sys;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// The command that starts a detached daemon for `project`, unspawned: `<rdy_bin> serve --detach
/// --spawned --project <project>` in `project` (`--spawned` tells `rdy serve --detach` to serve
/// in that process rather than spawn another), stdin from /dev/null, stdout and stderr appended
/// to the run dir's `daemon.log` (created 0600; refused if it is a symlink), every `RDY_*`
/// variable removed from its environment, and a new session (so a new process group) of its own
/// from before its exec. The run dir is made absolute first, and a socket path the daemon could
/// not bind (`socket::check_socket_path`) is refused on that absolute path, before anything is
/// created, rather than spawning a daemon that would only die at its bind. A relative `project`
/// is made absolute against the caller's working directory too: the child runs in the project,
/// where the relative path would name `<project>/<project>`.
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
        .current_dir(&project)
        .stdin(Stdio::null())
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

/// Whether a process `pid` exists (`kill(pid, 0)`; a process of another user counts).
pub fn process_alive(pid: u32) -> bool {
    sys::alive(pid)
}

/// Sends `SIGTERM` to `pid`, which `rdy serve` takes as `rdy serve --stop`. `Ok(true)` when it
/// was sent, `Ok(false)` when the process is gone (`ESRCH`). A `pid` of 1 or less is refused:
/// never init, and never a process group (`kill(0, ..)` and negative pids).
pub fn terminate(pid: u32) -> io::Result<bool> {
    sys::terminate(pid)
}

/// Leaves the caller's session (`setsid`). `EPERM` means the caller already leads a process
/// group (a child of `serve_command` already leads its own session), and is `Ok`. Called by
/// `rdy serve --detach` after its argument parsing and before it opens the store.
pub fn detach() -> io::Result<()> {
    sys::detach()
}

/// The caller's process group id (`getpgid(0)`), for the tests.
#[doc(hidden)]
pub fn own_pgid() -> i32 {
    sys::own_pgid()
}

/// The caller's session id (`getsid(0)`), for the tests.
#[doc(hidden)]
pub fn own_sid() -> i32 {
    sys::own_sid()
}
