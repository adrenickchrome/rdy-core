//! A stop on Windows: the project's named event, and the proved end of a process.
//!
//! - **The event.** `Local\rdy-stop-<hash>`, with the hash of the project's pipe. It is private to
//!   this account from the moment it is made, like the pipe. An event of that name that already
//!   exists is another process's, whoever made it and however, so it is refused: a daemon never
//!   waits on an event someone else can set.
//! - **The fallback only.** The usual stop is the ctl op. A daemon of another version with other
//!   clients refuses every hello, and only then is the event set.
//! - **The handle comes first.** The peer check took a handle, which keeps the pid from going to
//!   another process. A second handle, with the right to end the process, is taken before the
//!   signal goes. Everything after that acts on these handles and never on a pid.
//!   [`stop_proof`](super::super::stop_proof) decides what happens, and when.
//! - **Local, so one session.** `Local\` is one terminal session's namespace, and a pipe's name is
//!   not. A client in another session finds no event and reports a failed signal.

use super::super::records::{stop_event_name, stop_event_of_pipe};
use super::super::stop_proof::{Refusal, StopBoundary, StopOutcome, stop_refused_version};
use super::agent::Event;
use super::identity::{PeerProcess, creation_time};
use super::process::{open_process, wait_exit};
use super::run_state::project_hash;
use crate::layout::RunDir;
use rdy_store::pio::{ObjectKind, PrivateDescriptor};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::signal::windows::{CtrlBreak, CtrlC, ctrl_break, ctrl_c};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, FALSE, GetLastError, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, SetEvent, TerminateProcess, WaitForMultipleObjects,
};

/// The exit code of a daemon that was ended: as a shell reports a stop by `SIGTERM`.
pub const FORCED_EXIT_CODE: u32 = 143;

fn wide_name(name: &str) -> Vec<u16> {
    std::ffi::OsStr::new(name)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

/// Makes the event `name`, private to this account. One signal wakes one waiter, and stays set
/// until then, so no signal is lost before the wait starts. An event of that name that exists is
/// refused: `could not create stop event {event}: {error}`.
#[allow(unsafe_code)]
fn create_stop_event(name: &str) -> io::Result<OwnedHandle> {
    let refused =
        |e: io::Error| io::Error::new(e.kind(), format!("could not create stop event {name}: {e}"));
    let descriptor = PrivateDescriptor::of_this_account(ObjectKind::File).map_err(refused)?;
    let attributes = descriptor.attributes();
    let wide = wide_name(name);
    // SAFETY: `attributes` and its descriptor, and the name, are valid and outlive the call.
    let event = unsafe { CreateEventW(&attributes, FALSE, FALSE, wide.as_ptr()) };
    if event.is_null() {
        return Err(refused(io::Error::last_os_error()));
    }
    // SAFETY: the call just above set it, and nothing has run since.
    let last = unsafe { GetLastError() };
    // SAFETY: the call succeeded, so `event` is a new handle that nothing else owns.
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    if last == ERROR_ALREADY_EXISTS {
        return Err(refused(io::Error::from_raw_os_error(last as i32)));
    }
    Ok(event)
}

/// Sets the stop event `name`, once: this account's right, and no other's. The error is the
/// system's.
#[allow(unsafe_code)]
pub fn signal_stop_event(name: &str) -> io::Result<()> {
    let wide = wide_name(name);
    // SAFETY: the name is NUL-terminated and outlives the call.
    let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, FALSE, wide.as_ptr()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so `event` is a new handle that nothing else owns.
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    // SAFETY: the handle is open for the call, with `EVENT_MODIFY_STATE`.
    if unsafe { SetEvent(event.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Test hook: sets the stop event of the project at the canonical `root`. `Ok(true)` when it stood;
/// an event that does not stand is the system's error.
#[doc(hidden)]
pub fn signal_project_stop(root: &Path) -> io::Result<bool> {
    signal_stop_event(&stop_event_name(&project_hash(root))).map(|()| true)
}

/// The thread that waits on the event and tells [`StopSignals::recv`]. It holds the event, so the
/// name lasts as long as this does.
struct EventWaiter {
    quit: Arc<Event>,
    thread: Option<JoinHandle<()>>,
}

impl EventWaiter {
    fn start(event: OwnedHandle, heard: UnboundedSender<()>) -> io::Result<EventWaiter> {
        let quit = Arc::new(Event::new()?);
        let thread = {
            let quit = Arc::clone(&quit);
            std::thread::Builder::new()
                .name("rdy-stop-event".into())
                .spawn(move || wait_on_event(&event, &quit, &heard))?
        };
        Ok(EventWaiter {
            quit,
            thread: Some(thread),
        })
    }
}

impl Drop for EventWaiter {
    fn drop(&mut self) {
        self.quit.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Tells `heard` of each signal, until the quit is set or nobody listens. A wait that fails also
/// ends this: the daemon then hears no event, and its other stops still work.
#[allow(unsafe_code)]
fn wait_on_event(event: &OwnedHandle, quit: &Event, heard: &UnboundedSender<()>) {
    let handles = [event.as_raw_handle(), quit.raw()];
    loop {
        // SAFETY: both handles stay open for the call: this function's caller holds them, and the
        // array holds exactly the count given.
        let woke = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), FALSE, INFINITE) };
        if woke != WAIT_OBJECT_0 || heard.send(()).is_err() {
            return;
        }
    }
}

/// The project's stop event, Ctrl-C and Ctrl-Break, registered in the current tokio runtime.
pub struct StopSignals {
    heard: UnboundedReceiver<()>,
    tell: UnboundedSender<()>,
    /// Holds the event, once [`claim_project`](Self::claim_project) has made it.
    waiter: Option<EventWaiter>,
    ctrl_c: CtrlC,
    ctrl_break: CtrlBreak,
}

impl StopSignals {
    /// Whether a daemon may not start without these: a client's fallback stop needs the event.
    pub const REQUIRED: bool = true;

    /// Makes the stop event of the project at the canonical `root` and registers the console's
    /// handlers in the current tokio runtime; call it inside one.
    pub fn install(root: &Path) -> io::Result<StopSignals> {
        let mut signals = Self::early()?;
        signals.claim_project(root)?;
        Ok(signals)
    }

    /// Registers the console's handlers in the current tokio runtime; call it inside one. This goes
    /// before the store opens, and holds no name.
    pub fn early() -> io::Result<StopSignals> {
        let (tell, heard) = unbounded_channel();
        Ok(StopSignals {
            heard,
            tell,
            waiter: None,
            ctrl_c: ctrl_c()?,
            ctrl_break: ctrl_break()?,
        })
    }

    /// Makes the stop event of the project at the canonical `root`. A daemon does this once it holds
    /// the store, so a project's own live daemon is refused first, in the store's words. The event
    /// stays until this value is dropped.
    pub fn claim_project(&mut self, root: &Path) -> io::Result<()> {
        let event = create_stop_event(&stop_event_name(&project_hash(root)))?;
        self.waiter = Some(EventWaiter::start(event, self.tell.clone())?);
        Ok(())
    }

    /// Waits for the next signal and names it: "the stop event", "Ctrl-C" or "Ctrl-Break".
    pub async fn recv(&mut self) -> &'static str {
        tokio::select! {
            Some(()) = self.heard.recv() => "the stop event",
            _ = self.ctrl_c.recv() => "Ctrl-C",
            _ = self.ctrl_break.recv() => "Ctrl-Break",
        }
    }
}

/// A daemon's process, held since the peer check, and what a stop reads and does to it.
struct Retained {
    run: RunDir,
    endpoint: PathBuf,
    process: PeerProcess,
    /// A second handle with the right to end it, or the number of the error that refused it.
    ender: Result<OwnedHandle, i32>,
}

impl Retained {
    /// Takes the handle with the right to end the process. The handle already held keeps the pid
    /// from going to another process, so this opens the same one, and [`held_created`] checks
    /// that. A refusal changes nothing yet: a daemon that heeds the signal never needs the right.
    ///
    /// [`held_created`]: StopBoundary::held_created
    fn capture(run: RunDir, endpoint: PathBuf, process: PeerProcess) -> Retained {
        let ender = process.pid().ok_or(0).and_then(|pid| {
            open_process(
                pid,
                PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            )
            .map_err(|e| e.raw_os_error().unwrap_or(0))
        });
        Retained {
            run,
            endpoint,
            process,
            ender,
        }
    }
}

impl StopBoundary for Retained {
    fn record(&self) -> Option<PathBuf> {
        self.run.recorded_socket()
    }

    fn pid_file(&self) -> Option<(u32, u64)> {
        self.run.pid_record().map(|r| (r.pid(), r.created()))
    }

    fn held_created(&self) -> io::Result<u64> {
        let held = creation_time(self.process.handle().as_raw_handle())?;
        if let Ok(ender) = &self.ender
            && creation_time(ender.as_raw_handle())? != held
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(held)
    }

    fn has_exited(&self) -> io::Result<bool> {
        wait_exit(self.process.handle().as_raw_handle(), 0)
    }

    fn signal(&self) -> io::Result<()> {
        // the pipe's own name: a project that has moved keeps the event its daemon made
        let name = stop_event_of_pipe(&self.endpoint).ok_or(io::ErrorKind::InvalidInput)?;
        signal_stop_event(&name)
    }

    fn wait_exit(&self, grace: Duration) -> io::Result<bool> {
        // INFINITE is the one value that is not a time; a longer wait stops just short of it
        let ms = u32::try_from(grace.as_millis())
            .unwrap_or(INFINITE)
            .min(INFINITE - 1);
        wait_exit(self.process.handle().as_raw_handle(), ms)
    }

    #[allow(unsafe_code)]
    fn terminate(&self) -> io::Result<()> {
        let ender = self
            .ender
            .as_ref()
            .map_err(|&code| io::Error::from_raw_os_error(code))?;
        // SAFETY: the handle is open for the call, with `PROCESS_TERMINATE`.
        if unsafe { TerminateProcess(ender.as_raw_handle(), FORCED_EXIT_CODE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Stops a daemon of another version that refused a connection to `endpoint`, whose peer check gave
/// `process`: [`stop_refused_version`], on a thread that may block for `grace`. If the caller gives
/// up, the thread goes on: a stop was asked for, and the proofs are the same.
pub async fn stop_refused_daemon(
    run: RunDir,
    endpoint: PathBuf,
    process: PeerProcess,
    grace: Duration,
) -> StopOutcome {
    tokio::task::spawn_blocking(move || {
        let refusal = Refusal {
            endpoint: endpoint.clone(),
            pid: process.pid(),
            created: process.created(),
        };
        let retained = Retained::capture(run, endpoint, process);
        stop_refused_version(&retained, &refusal, grace)
    })
    .await
    .unwrap_or(StopOutcome::Untied)
}
