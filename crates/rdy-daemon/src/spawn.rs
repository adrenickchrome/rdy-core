//! Starting a detached daemon: the `rdy serve --detach` command a client spawns, and the
//! child's own `detach`.

use crate::layout::RunDir;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// The command that starts a detached daemon for `project`, unspawned: `<rdy_bin> serve --detach
/// --project <project>` in `project`, stdin from /dev/null, stdout and stderr appended to the run
/// dir's `daemon.log` (created 0600), every `RDY_*` variable removed from its environment, and
/// a new session (so a new process group) of its own from before its exec.
pub fn serve_command(rdy_bin: &Path, project: &Path, run: &RunDir) -> io::Result<Command> {
    run.ensure()?;
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(run.log_file())?;
    // a file left by an older run keeps its mode through open; set it exactly
    log.set_permissions(fs::Permissions::from_mode(0o600))?;
    let (out, err) = (log.try_clone()?, log.try_clone()?);
    let mut cmd = Command::new(rdy_bin);
    cmd.arg("serve")
        .arg("--detach")
        .arg("--project")
        .arg(project)
        .current_dir(project)
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

/// Spawns `cmd` and returns without waiting for it to finish. A thread reaps the child when it
/// exits, so a long-lived spawner leaves no zombie behind.
pub fn spawn_serve(cmd: &mut Command) -> io::Result<()> {
    let mut child = cmd.spawn()?;
    std::thread::Builder::new()
        .name("rdy-serve-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
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
