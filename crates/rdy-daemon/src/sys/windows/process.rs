//! Processes on Windows: a detached start, exit codes and liveness.
//!
//! - **A detached daemon holds nothing of its starter.** It has no console, so it outlives the
//!   invoking one and hears none of its events. It has a process group of its own. No handle of
//!   this process goes with it, unless it is one of the three the command names. Files, pipes and
//!   the store's handles are never inheritable here. This process's own standard handles may be,
//!   when its parent passed them so: [`new_session_on_spawn`] takes that away first, or a daemon
//!   would keep a host's stdout open, and the host would never read its end.
//! - **Only the system's word counts for liveness.** Opening a process and looking at it gives
//!   what [`Liveness`] says. No process is ended here: `stop` does that, through a retained handle,
//!   and only with its proofs. There is no `terminate` by pid on this platform.

use super::super::Liveness;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::CommandExt;
use std::process::{Command, ExitStatus};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Console::{
    FreeConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

/// Gives the child that `cmd` spawns no console and a process group of its own, and keeps this
/// process's standard handles from going with it. The command's own stdin, stdout and stderr are
/// set as it says: the standard library gives a child a copy made for it, so other children of
/// this process lose nothing.
#[allow(unsafe_code)]
pub fn new_session_on_spawn(cmd: &mut Command) {
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: the call takes a number and touches no memory of ours.
        let handle = unsafe { GetStdHandle(which) };
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            // SAFETY: the system gave this handle for this process. A handle that takes no flag
            // refuses, and is then no concern of a child's: the result changes nothing here.
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
    }
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// The exit code of `status`: this platform always has one.
pub fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// Waits on the process `process` is open on, with `SYNCHRONIZE`, for at most `ms`: whether it has
/// ended.
#[allow(unsafe_code)]
pub(super) fn wait_exit(process: RawHandle, ms: u32) -> io::Result<bool> {
    // SAFETY: the caller keeps `process` open for the call.
    match unsafe { WaitForSingleObject(process, ms) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        other => Err(io::Error::from_raw_os_error(other as i32)),
    }
}

/// Opens the process `pid` with `access`. The error is the system's.
#[allow(unsafe_code)]
pub(super) fn open_process(pid: u32, access: u32) -> io::Result<OwnedHandle> {
    // SAFETY: the call takes no pointer.
    let handle = unsafe { OpenProcess(access, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so `handle` is a new handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// What the system says of `pid`. An id that names no process, and a process that has ended, are
/// gone; an open and a look that say it runs, and access denied (another account's process), are
/// live; and anything else proves neither. Pid 0 is no process that can be asked about: the system
/// answers for it as for an id that names none, which proves nothing.
pub fn process_liveness(pid: u32) -> Liveness {
    if pid == 0 {
        return Liveness::Ambiguous;
    }
    let process = match open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE) {
        Ok(process) => process,
        Err(e) => {
            return match e.raw_os_error().and_then(|n| u32::try_from(n).ok()) {
                Some(ERROR_INVALID_PARAMETER) => Liveness::Gone,
                Some(ERROR_ACCESS_DENIED) => Liveness::Live,
                _ => Liveness::Ambiguous,
            };
        }
    };
    match wait_exit(process.as_raw_handle(), 0) {
        Ok(true) => Liveness::Gone,
        Ok(false) => Liveness::Live,
        Err(_) => Liveness::Ambiguous,
    }
}

/// Whether a process `pid` may exist: everything but a proved end, so a process of another account
/// counts.
pub fn alive(pid: u32) -> bool {
    process_liveness(pid) != Liveness::Gone
}

/// Leaves the caller's console. A process with none (a child of `serve_command`) has nothing to
/// leave, and is `Ok`. Called by `rdy serve --detach` after its argument parsing and before it
/// opens the store.
#[allow(unsafe_code)]
pub fn detach() -> io::Result<()> {
    // SAFETY: the call takes nothing.
    if unsafe { FreeConsole() } != 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error().and_then(|n| u32::try_from(n).ok()) {
        Some(ERROR_INVALID_PARAMETER | ERROR_INVALID_HANDLE) => Ok(()),
        _ => Err(e),
    }
}
