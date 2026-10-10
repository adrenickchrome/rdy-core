//! Starting a detached daemon: the `rdy serve --detach` command a client spawns, and the
//! child's own `detach`.

use crate::layout::RunDir;
use crate::lock::open_no_follow;
use crate::socket::check_socket_path;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
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
        OpenOptions::new().append(true).create(true).mode(0o600),
        &run.log_file(),
    )?;
    // a file left by an older run keeps its mode through open; set it exactly
    log.set_permissions(fs::Permissions::from_mode(0o600))?;
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
                let c = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
            }
        })?;
    Ok(Spawned {
        exited: Box::new(move || *code.lock().unwrap_or_else(|e| e.into_inner())),
    })
}

/// Whether a process `pid` exists (`kill(pid, 0)`; a process of another user counts).
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; `kill` only checks that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Leaves the caller's session (`setsid`). `EPERM` means the caller already leads a process
/// group (a child of `serve_command` already leads its own session), and is `Ok`. Called by
/// `rdy serve --detach` after its argument parsing and before it opens the store.
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
pub fn own_pgid() -> i32 {
    // SAFETY: `getpgid(0)` asks about the caller and touches no memory of ours.
    unsafe { libc::getpgid(0) }
}

/// The caller's session id (`getsid(0)`), for the tests.
#[doc(hidden)]
pub fn own_sid() -> i32 {
    // SAFETY: `getsid(0)` asks about the caller and touches no memory of ours.
    unsafe { libc::getsid(0) }
}
