//! The proofs behind the stop of a daemon of another version, as pure logic over a
//! [`StopBoundary`], so that every platform's tests run every path and none waits out a grace
//! period (D-FPKPS). `sys::windows` is the product caller of the whole chain. Unix signals and
//! never forces, so it uses only [`proved`].
//!
//! A daemon of another version with other clients refuses every hello, so a stop cannot go over
//! its endpoint. What may be done to its process depends on four proofs:
//!
//! 1. `daemon.sock` is valid for this `.rdy` and names the endpoint that refused;
//! 2. the system gives the pid at the other end of that connection, and it is above 1;
//! 3. `daemon.pid` names that pid;
//! 4. `daemon.pid` and the retained process handle both give the connection's creation time.
//!
//! All four come before the signal, and again just before a force. A missing or changed proof
//! sends nothing. A handle that cannot answer proves nothing. The one other way to end with no
//! force: the daemon has removed both run files, so it heard the stop and drains. Nothing forces
//! a drain, and the caller goes on waiting for it.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What the connection that was refused showed: the system's word, read before anything was sent.
#[derive(Debug)]
pub struct Refusal {
    /// The endpoint the connection went to.
    pub endpoint: PathBuf,
    /// The pid at its other end, `None` when the system does not say.
    pub pid: Option<u32>,
    /// That process's creation time. Unix has none and gives 0 here and in the pid file.
    pub created: u64,
}

/// What a stop reads from and does to the system. A test scripts it, so the grace period's wait is
/// one call that returns at once.
pub trait StopBoundary {
    /// The endpoint `daemon.sock` names, when it is valid for this `.rdy`.
    fn record(&self) -> Option<PathBuf>;
    /// The pid and the creation time `daemon.pid` gives.
    fn pid_file(&self) -> Option<(u32, u64)>;
    /// The creation time of the process the retained handle holds, read through that handle.
    fn held_created(&self) -> io::Result<u64>;
    /// Whether the process the retained handle holds has ended.
    fn has_exited(&self) -> io::Result<bool>;
    /// Asks the daemon to stop.
    fn signal(&self) -> io::Result<()>;
    /// Waits for at most `grace`: whether the process ended in that time.
    fn wait_exit(&self, grace: Duration) -> io::Result<bool>;
    /// Ends the process the retained handle holds.
    fn terminate(&self) -> io::Result<()>;
}

/// What a stop did.
#[derive(Debug)]
pub enum StopOutcome {
    /// A proof was missing or changed. Nothing more was sent.
    Untied,
    /// The process had ended before anything was sent.
    Exited { pid: u32 },
    /// The signal went. This platform never forces, and its caller waits for the exit.
    Signalled { pid: u32 },
    /// The signal went, and the process ended within the grace period.
    Stopped { pid: u32 },
    /// The signal went, and the process is alive with both its run files removed: it drains.
    Draining { pid: u32 },
    /// The grace period ended with every proof holding, and the process was ended.
    Terminated { pid: u32 },
    /// The signal could not be sent. Nothing was forced.
    SignalFailed { pid: u32, error: io::Error },
    /// The process could not be ended.
    TerminateFailed { pid: u32, error: io::Error },
}

/// The pid that `refusal` and the rest of the proofs tie together, or `None`. `held` is the
/// creation time the retained handle gives, `None` when it cannot say.
pub fn proved(
    refusal: &Refusal,
    record: Option<&Path>,
    pid_file: Option<(u32, u64)>,
    held: Option<u64>,
) -> Option<u32> {
    let pid = refusal.pid.filter(|&p| p > 1)?;
    let holds = record == Some(refusal.endpoint.as_path())
        && pid_file == Some((pid, refusal.created))
        && held == Some(refusal.created);
    holds.then_some(pid)
}

fn proved_now<B: StopBoundary>(system: &B, refusal: &Refusal) -> bool {
    proved(
        refusal,
        system.record().as_deref(),
        system.pid_file(),
        system.held_created().ok(),
    )
    .is_some()
}

/// The whole stop: the proofs, the signal, the grace period and, only with the proofs again, the
/// force. The handle `system` holds was taken before this runs, so everything here acts on the
/// process the connection showed, never on a pid that has come free.
pub fn stop_refused_version<B: StopBoundary>(
    system: &B,
    refusal: &Refusal,
    grace: Duration,
) -> StopOutcome {
    let Some(pid) = refusal.pid.filter(|&p| p > 1) else {
        return StopOutcome::Untied;
    };
    match system.has_exited() {
        Ok(false) => {}
        Ok(true) => return StopOutcome::Exited { pid },
        Err(_) => return StopOutcome::Untied,
    }
    if !proved_now(system, refusal) {
        return StopOutcome::Untied;
    }
    if let Err(error) = system.signal() {
        // a daemon that has just left has taken its event with it
        return match system.has_exited() {
            Ok(true) => StopOutcome::Exited { pid },
            _ => StopOutcome::SignalFailed { pid, error },
        };
    }
    match system.wait_exit(grace) {
        Ok(false) => {}
        Ok(true) => return StopOutcome::Stopped { pid },
        Err(_) => return StopOutcome::Untied,
    }
    match system.has_exited() {
        Ok(false) => {}
        Ok(true) => return StopOutcome::Stopped { pid },
        Err(_) => return StopOutcome::Untied,
    }
    if system.record().is_none() && system.pid_file().is_none() {
        return StopOutcome::Draining { pid };
    }
    if !proved_now(system, refusal) {
        return StopOutcome::Untied;
    }
    match system.terminate() {
        Ok(()) => StopOutcome::Terminated { pid },
        // a process that ended in this instant refuses too
        Err(error) => match system.has_exited() {
            Ok(true) => StopOutcome::Stopped { pid },
            _ => StopOutcome::TerminateFailed { pid, error },
        },
    }
}
