//! Processes on Windows: liveness, and the Job Object that owns a child and all it starts.
//!
//! - **A child is in its job before it runs.** A job holds only what starts after a process
//!   enters it, so a child that ran first could leave a descendant outside. [`spawn_in_job`]
//!   makes the child suspended, puts it in a new job and only then lets it run. The standard
//!   library keeps the child's first thread to itself, so the thread is found by the process id,
//!   which cannot be reused while this process holds the child.
//! - **The job's owner owns the tree.** A job ends all it holds when its last handle closes, and
//!   its handle goes to no child. So a [`Job`] that is dropped, for whatever reason (this process
//!   ending too), leaves nothing running. It gives no breakaway, so no descendant can leave.
//! - **A job nests.** A process that is already in a job gets a job inside it. It stays in the
//!   outer one: ending the inner job leaves the outer's other processes, and ending the outer job
//!   ends the inner's too. No start here asks to break away.
//! - **A child that is refused never runs.** It is ended while still suspended, and the failure
//!   is the start's result, unless the cleanup itself failed: a cleanup failure comes first, as
//!   in the bounded runner.

use rdy_daemon::sys::{Liveness, process_liveness};
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_MORE_FILES, ERROR_OPERATION_ABORTED,
    GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateEventW, INFINITE, OpenProcess, OpenThread, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE, ResumeThread, SetEvent, THREAD_SUSPEND_RESUME, TerminateProcess,
    WaitForMultipleObjects, WaitForSingleObject,
};

pub use rdy_store::pio::{ProgramInDir, StartPin, program_in_dir, start_in_dir};

/// The exit code of a process this module ended: 128 plus SIGKILL's number, as a shell shows it.
pub const KILLED_EXIT_CODE: u32 = 137;

/// Whether process `pid` has exited: no process has this id, or it has ended and only a handle
/// keeps its record (killed, not yet reaped by its parent). Panics if the system does not say.
pub fn process_exited(pid: u32) -> bool {
    match process_liveness(pid) {
        Liveness::Gone => true,
        Liveness::Live => false,
        Liveness::Ambiguous => panic!("the system does not say whether process {pid} exited"),
    }
}

/// Whether process `pid` is alive. A process this account cannot open counts, as it may be
/// running; the harness only asks about its own children.
pub fn process_running(pid: u32) -> bool {
    process_liveness(pid) == Liveness::Live
}

/// Ends process `pid` at once, with [`KILLED_EXIT_CODE`]. A process that has already ended and
/// still has a record is left as it is; panics if the process cannot be opened or ended.
pub fn kill_hard(pid: u32) {
    let process = open_process(pid, PROCESS_TERMINATE | PROCESS_SYNCHRONIZE)
        .unwrap_or_else(|e| panic!("open process {pid}: {e}"));
    if let Err(e) = terminate(&process)
        && !has_ended(&process)
    {
        panic!("end process {pid}: {e}");
    }
}

/// Opens the process `pid` with `access`. The error is the system's.
#[allow(unsafe_code)]
fn open_process(pid: u32, access: u32) -> io::Result<OwnedHandle> {
    // SAFETY: the call takes no pointer.
    let handle = unsafe { OpenProcess(access, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so `handle` is a new handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// Ends the process `process` is open on.
#[allow(unsafe_code)]
fn terminate(process: &OwnedHandle) -> io::Result<()> {
    // SAFETY: `process` is open for the call.
    if unsafe { TerminateProcess(process.as_raw_handle(), KILLED_EXIT_CODE) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether the process `process` is open on, with `SYNCHRONIZE`, has ended.
#[allow(unsafe_code)]
fn has_ended(process: &OwnedHandle) -> bool {
    // SAFETY: `process` is open for the call.
    unsafe { WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_OBJECT_0 }
}

/// A Job Object: it holds a process and all that process starts, and ends them all when its last
/// handle closes.
#[derive(Debug)]
pub struct Job(OwnedHandle);

impl Job {
    /// A new, empty job with no name, whose handle no child inherits.
    #[allow(unsafe_code)]
    pub fn new() -> io::Result<Self> {
        // SAFETY: both pointers may be null: default attributes, which are not inheritable, and
        // no name.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call succeeded, so `handle` is a new handle that nothing else owns.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        // SAFETY: the struct is plain data, for which all zeros is valid: no limit set.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the job is open, and the pointer and size describe `limits`, the struct this
        // class takes, which outlives the call.
        let set = unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast::<c_void>(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Puts the process `process` is open on in this job.
    #[allow(unsafe_code)]
    fn assign(&self, process: &impl AsRawHandle) -> io::Result<()> {
        // SAFETY: the job and the process are open for the call.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process.as_raw_handle()) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Whether the process `process` is open on is in this job, or in a job inside it.
    #[allow(unsafe_code)]
    pub fn holds(&self, process: &impl AsRawHandle) -> io::Result<bool> {
        let mut held = 0;
        // SAFETY: the job and the process are open for the call, and `held` outlives it.
        let asked = unsafe {
            IsProcessInJob(
                process.as_raw_handle(),
                self.0.as_raw_handle(),
                &raw mut held,
            )
        };
        if asked == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(held != 0)
    }

    /// How many processes run in this job and the jobs inside it.
    #[allow(unsafe_code)]
    pub fn active_processes(&self) -> io::Result<u32> {
        // SAFETY: the struct is plain data, for which all zeros is valid.
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the job is open, and the pointer and size describe `info`, the struct this
        // class gives, which outlives the call. The returned length may be null.
        let asked = unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast::<c_void>(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        };
        if asked == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info.ActiveProcesses)
    }

    /// Ends every process in this job and the jobs inside it, each with [`KILLED_EXIT_CODE`]. A
    /// job with nothing left in it is already stopped, which is success.
    #[allow(unsafe_code)]
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: the job is open for the call.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), KILLED_EXIT_CODE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// A running child and the job that holds it. Dropping this ends the child and all it started.
#[derive(Debug)]
pub struct JobChild {
    child: Child,
    job: Job,
}

impl JobChild {
    /// The child's process id.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// The child, for its streams and its wait.
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// The job that holds the child.
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// The child and its job, for a caller that waits and ends on separate threads. The job
    /// stays the owner: dropping it ends the child.
    pub fn into_parts(self) -> (Child, Job) {
        (self.child, self.job)
    }
}

/// Starts `command` with a new [`Job`] holding the child from before its first instruction. This
/// sets the command's creation flags, so any set earlier are lost. `NotFound` and every other
/// spawn failure keep their kind, and a failed start leaves no process and no handle behind.
pub fn spawn_in_job(command: &mut Command) -> io::Result<JobChild> {
    start_in_job(command, &mut |_, _, _| Ok(()))
}

/// Where a start stands when its hook runs. The child has run nothing at either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// The child is made and suspended, and in no job of this start.
    BeforeAssign,
    /// The job holds the child, which is still suspended.
    BeforeResume,
}

/// What [`start_in_job`] calls at each [`Stage`]; a failure refuses the start, and tests use it.
type StartHook<'a> = dyn FnMut(Stage, &Child, &Job) -> io::Result<()> + 'a;

/// [`spawn_in_job`] with a hook at each [`Stage`].
fn start_in_job(command: &mut Command, at: &mut StartHook) -> io::Result<JobChild> {
    let job = Job::new()?;
    command.creation_flags(CREATE_SUSPENDED);
    let mut child = command.spawn()?;
    let ready = at(Stage::BeforeAssign, &child, &job)
        .and_then(|()| job.assign(&child))
        .and_then(|()| at(Stage::BeforeResume, &child, &job))
        .and_then(|()| resume(child.id()));
    match ready {
        Ok(()) => Ok(JobChild { child, job }),
        Err(failure) => {
            // The job ends what a part-way resume let start, and the kill a child it never held.
            let ended = job.terminate().and(child.kill());
            let cleanup = ended.and_then(|()| child.wait().map(drop));
            Err(cleanup.err().unwrap_or(failure))
        }
    }
}

/// Lets the suspended process `pid` run.
#[allow(unsafe_code)]
fn resume(pid: u32) -> io::Result<()> {
    let threads = for_each_thread(pid, |thread| {
        // SAFETY: `thread` is open with THREAD_SUSPEND_RESUME for the call.
        if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })?;
    if threads == 0 {
        return Err(io::Error::other(format!(
            "process {pid} has no thread to resume"
        )));
    }
    Ok(())
}

/// Calls `f` with each thread of process `pid`, open with `THREAD_SUSPEND_RESUME`; how many there
/// were.
#[allow(unsafe_code)]
fn for_each_thread(
    pid: u32,
    mut f: impl FnMut(&OwnedHandle) -> io::Result<()>,
) -> io::Result<usize> {
    // SAFETY: the call takes no pointer. A thread snapshot lists every process's threads.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so `snapshot` is a new handle that nothing else owns.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    // SAFETY: the struct is plain data, for which all zeros is valid.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut threads = 0;
    // SAFETY: the snapshot is open, and `entry` is a sized struct that outlives the call.
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) };
    while more != 0 {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: the call takes no pointer.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the call succeeded, so `thread` is a new handle that nothing else owns.
            f(&unsafe { OwnedHandle::from_raw_handle(thread) })?;
            threads += 1;
        }
        // SAFETY: as for the first call.
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) };
    }
    let end = io::Error::last_os_error();
    if end.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        return Err(end);
    }
    Ok(threads)
}

/// The output cap of a release tool's child process.
pub const RELEASE_CHILD_OUTPUT_MAX: usize = 1_048_576;
/// The hang guard of a release tool's child process.
pub const RELEASE_CHILD_DEADLINE: Duration = Duration::from_secs(30);

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
    /// The direct child's process id, once it started.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) pid: Option<u32>,
}

/// Runs `command` in a [`Job`] of its own, with stdin from `stdin` (null when `None`) and stdout
/// and stderr captured under one combined cap of `max_output` bytes.
///
/// When the direct child exits, the job is ended, so a descendant that still holds a stream
/// cannot keep a drain running. On overflow, at `deadline`, on a read error or a setup failure,
/// the job is ended too; the runner then waits for the direct child, cancels both drains and
/// joins them. A drain's blocked read ends when the runner cancels it, so a process outside the
/// job that keeps a stream open cannot hold the runner past its deadline. Overflow is
/// `InvalidData`, the deadline `TimedOut`, and a setup, read or cleanup failure keeps its own
/// kind. A cleanup failure is the result even after an earlier failure, and never becomes
/// success. A nonzero exit status is an `Output`, for the caller to judge.
///
/// The streams are named pipes opened for overlapped reads: a read waits on the pipe and on the
/// runner's cancel signal together, which is what makes a drain cancellable.
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

/// A signal that stays set once it is set: the runner's cancellation of its drains.
#[derive(Debug)]
struct Signal(OwnedHandle);

impl Signal {
    #[allow(unsafe_code)]
    fn new() -> io::Result<Self> {
        // SAFETY: the pointers may be null: default attributes and no name. The event is
        // manual-reset and starts clear.
        let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call succeeded, so `handle` is a new handle that nothing else owns.
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    #[allow(unsafe_code)]
    fn set(&self) {
        // SAFETY: the event is open for the call.
        let done = unsafe { SetEvent(self.0.as_raw_handle()) };
        // A set that fails on an open event would leave a drain blocked for good.
        assert_ne!(
            done,
            0,
            "set the cancel signal: {}",
            io::Error::last_os_error()
        );
    }

    #[allow(unsafe_code)]
    fn is_set(&self) -> bool {
        // SAFETY: the event is open for the call.
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }
}

/// How many pipes this process has named, so no two names are alike.
static PIPE_NUMBER: AtomicUsize = AtomicUsize::new(0);

/// Held from the moment a runner's child ends are inheritable until its command is dropped. A
/// child inherits every inheritable handle of its parent, so two starts at once could give one
/// run's stream to another run's child, which would then hold it open.
static START: Mutex<()> = Mutex::new(());

/// What one overlapped read found.
enum Chunk {
    /// This many bytes arrived.
    Data(usize),
    /// Every writer has closed the pipe.
    End,
    /// The runner cancelled the read before it finished.
    Cancelled,
}

/// The runner's end of one captured stream.
struct PipeRead {
    pipe: OwnedHandle,
    /// Completes when a read does; only this end's drain waits on it.
    done: Signal,
}

impl PipeRead {
    /// A new pipe. The first value is the runner's end, opened for overlapped reads and not
    /// inheritable; the second is the child's end, a write-only handle of a plain open.
    #[allow(unsafe_code)]
    fn pair() -> io::Result<(Self, OwnedHandle)> {
        let name: Vec<u16> = format!(
            r"\\.\pipe\rdy-qa-{}-{}",
            std::process::id(),
            PIPE_NUMBER.fetch_add(1, Ordering::SeqCst)
        )
        .encode_utf16()
        .chain([0])
        .collect();
        // SAFETY: `name` ends in NUL and outlives the call; the attributes may be null (default,
        // not inheritable). The first-instance flag refuses a pipe that someone else made.
        let server = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                64 * 1024,
                0,
                null(),
            )
        };
        if server == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call succeeded, so `server` is a new handle that nothing else owns.
        let pipe = unsafe { OwnedHandle::from_raw_handle(server) };
        // SAFETY: `name` ends in NUL and outlives the call; the attributes and the template may
        // be null.
        let client = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        if client == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call succeeded, so `client` is a new handle that nothing else owns.
        let client = unsafe { OwnedHandle::from_raw_handle(client) };
        Ok((
            Self {
                pipe,
                done: Signal::new()?,
            },
            client,
        ))
    }

    /// Reads into `buf`, or stops at `cancel`. A read that `cancel` ends is cancelled and
    /// awaited before this returns, so the system has let go of `buf` by then.
    #[allow(unsafe_code)]
    fn read(&self, cancel: &Signal, buf: &mut [u8]) -> io::Result<Chunk> {
        // SAFETY: the struct is plain data, for which all zeros is valid.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = self.done.0.as_raw_handle();
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let mut got = 0u32;
        // SAFETY: the pipe is open for overlapped reads; `buf`, `got` and `ov` stay in place
        // until the read has completed or been cancelled and awaited, below.
        let started = unsafe {
            ReadFile(
                self.pipe.as_raw_handle(),
                buf.as_mut_ptr(),
                len,
                &raw mut got,
                &raw mut ov,
            )
        };
        if started != 0 {
            return Ok(if got == 0 {
                Chunk::End
            } else {
                Chunk::Data(got as usize)
            });
        }
        // SAFETY: the call takes no pointer.
        let code = unsafe { GetLastError() };
        match code {
            ERROR_IO_PENDING => {}
            ERROR_BROKEN_PIPE => return Ok(Chunk::End),
            _ => return Err(io::Error::from_raw_os_error(code as i32)),
        }
        let both = [self.done.0.as_raw_handle(), cancel.0.as_raw_handle()];
        // SAFETY: both events are open for the call, and `both` holds two handles.
        let woke = unsafe { WaitForMultipleObjects(2, both.as_ptr(), 0, INFINITE) };
        if woke == WAIT_OBJECT_0 {
            return Self::finish(&self.pipe, &ov, false);
        }
        // The cancel signal, or a failed wait: either way end the read and let go of the buffer
        // before anything else.
        // SAFETY: the pipe is open, and `ov` is the read that is pending.
        unsafe { CancelIoEx(self.pipe.as_raw_handle(), &raw const ov) };
        let outcome = Self::finish(&self.pipe, &ov, true);
        if woke != WAIT_OBJECT_0 + 1 {
            return outcome.and(Err(io::Error::other("waiting for a read failed")));
        }
        match outcome {
            Ok(Chunk::Data(n)) => Ok(Chunk::Data(n)),
            Ok(Chunk::End) => Ok(Chunk::End),
            Ok(Chunk::Cancelled) | Err(_) => Ok(Chunk::Cancelled),
        }
    }

    /// The result of the read `ov` started on `pipe`; `wait` waits for it to complete.
    #[allow(unsafe_code)]
    fn finish(pipe: &OwnedHandle, ov: &OVERLAPPED, wait: bool) -> io::Result<Chunk> {
        let mut got = 0u32;
        // SAFETY: the pipe is open, and `ov` and `got` outlive the call.
        let done =
            unsafe { GetOverlappedResult(pipe.as_raw_handle(), ov, &raw mut got, i32::from(wait)) };
        if done != 0 {
            return Ok(if got == 0 {
                Chunk::End
            } else {
                Chunk::Data(got as usize)
            });
        }
        // SAFETY: the call takes no pointer.
        match unsafe { GetLastError() } {
            ERROR_BROKEN_PIPE => Ok(Chunk::End),
            ERROR_OPERATION_ABORTED => Ok(Chunk::Cancelled),
            code => Err(io::Error::from_raw_os_error(code as i32)),
        }
    }
}

/// Starts the bounded run of `command`: see [`run_bounded`]. `stdout` is a file when given (as
/// [`run_bounded_to`]); `deadline` is absolute (`None`: none); `on_start` receives the run's
/// [`Expiry`] once the child and its workers started.
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
    let stream = || PipeRead::pair().map(|(ours, theirs)| (ours, Stdio::from(theirs)));
    let captured_out = match stdout {
        Some(file) => Ok((None, Stdio::from(file))),
        None => stream().map(|(ours, theirs)| (Some(ours), theirs)),
    };
    let (out, err) = match (captured_out, stream()) {
        (Ok(out), Ok(err)) => (out, err),
        (Err(e), _) | (_, Err(e)) => return not_started(e),
    };
    command
        .stdin(stdin.map_or_else(Stdio::null, Stdio::from))
        .stdout(out.1)
        .stderr(err.1);
    #[cfg(test)]
    RUNNER_STARTS.with(|n| n.set(n.get() + 1));
    let cancel = match Signal::new() {
        Ok(cancel) => Arc::new(cancel),
        Err(e) => return not_started(e),
    };
    let spawned = {
        let _one_start = START
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let spawned = spawn_in_job(&mut command);
        // the command holds the child's ends: drop them, or no drain would see the end of a stream
        drop(command);
        spawned
    };
    let (child, job) = match spawned {
        Ok(started) => started.into_parts(),
        Err(e) => return not_started(e),
    };
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    let count = Arc::new(AtomicUsize::new(0));
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
                        // a descendant may still hold a stream: end the whole job
                        Ok(s) => match job.terminate() {
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

    // Cleanup: end the job, reap the direct child, then cancel and join the drains, whose
    // blocked reads end at the cancel whoever still holds the streams.
    let mut cleanup: Option<io::Error> = None;
    if failure.is_some() {
        if let Err(e) = job.terminate() {
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
    cancel.set();
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
fn spawn_drain(
    stream: PipeRead,
    count: &Arc<AtomicUsize>,
    cancel: &Arc<Signal>,
    max_output: usize,
    tx: &mpsc::Sender<RunEvent>,
) -> io::Result<std::thread::JoinHandle<Vec<u8>>> {
    let (count, cancel, tx) = (count.clone(), cancel.clone(), tx.clone());
    std::thread::Builder::new()
        .name("rdy-qa-drain".into())
        .spawn(move || drain_bounded(&stream, &count, &cancel, max_output, &tx))
}

/// Reads `stream` to its end, keeping each byte only while the count both streams share stays
/// within `max_output`; a byte is counted before it is kept. A blocked read ends at `cancel`,
/// which stops the drain. Reports overflow or a read error, then stops; always reports that it
/// is done.
fn drain_bounded(
    stream: &PipeRead,
    count: &AtomicUsize,
    cancel: &Signal,
    max_output: usize,
    tx: &mpsc::Sender<RunEvent>,
) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    while !cancel.is_set() {
        match stream.read(cancel, &mut buf) {
            Ok(Chunk::End | Chunk::Cancelled) => break,
            Ok(Chunk::Data(n)) => {
                let before = count.fetch_add(n, Ordering::SeqCst);
                let room = max_output.saturating_sub(before).min(n);
                kept.extend_from_slice(&buf[..room]);
                if before.saturating_add(n) > max_output {
                    let _ = tx.send(RunEvent::Overflow);
                    break;
                }
            }
            Err(e) => {
                let _ = tx.send(RunEvent::PipeError(e));
                break;
            }
        }
    }
    let _ = tx.send(RunEvent::DrainDone);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::ChildStdout;
    use windows_sys::Win32::Foundation::{
        DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ACCESS_DENIED, HANDLE,
    };
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, GetCurrentProcess, GetCurrentProcessId, GetProcessHandleCount,
        PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION, SuspendThread,
    };

    /// What a test's own executable does when a test starts it again: a mode names the part it
    /// plays, and no mode means the test itself.
    const MODE: &str = "RDY_WX23A_MODE";
    /// Where the `mark` part writes that it ran.
    const MARKER: &str = "RDY_WX23A_MARKER";
    /// A hang guard, never a speed check.
    const HANG_GUARD: Duration = Duration::from_secs(60);

    fn mode() -> Option<String> {
        std::env::var(MODE).ok()
    }

    /// A command that starts the test `name` of this module again, alone, as `mode`.
    fn this_test(name: &str, mode: &str) -> Command {
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        let mut cmd = Command::new(std::env::current_exe().expect("this executable"));
        cmd.args([
            "--exact",
            &format!("{module}::{name}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MODE, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
        cmd
    }

    /// Stays until the job that owns this process ends it.
    fn stay() -> ! {
        loop {
            std::thread::park();
        }
    }

    /// Writes one line a test reads. The line break before it ends libtest's own `test ... `.
    fn say(line: &str) {
        println!("\n{line}");
    }

    /// The lines of a part's stdout that it said with [`say`], under the hang guard.
    struct Said(mpsc::Receiver<String>);

    impl Said {
        fn of(stdout: ChildStdout) -> Self {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if line.starts_with("wx23a-") {
                        let _ = tx.send(line);
                    }
                }
            });
            Self(rx)
        }

        fn next(&self) -> String {
            self.0
                .recv_timeout(HANG_GUARD)
                .expect("a line from the part")
        }

        /// The process a `wx23a-leaf <pid>` line names, open before anything can reuse its id.
        fn leaf(&self) -> OwnedHandle {
            let line = self.next();
            let pid = line
                .strip_prefix("wx23a-leaf ")
                .and_then(|pid| pid.parse().ok())
                .unwrap_or_else(|| panic!("not a leaf: {line:?}"));
            open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)
                .expect("open the leaf")
        }
    }

    /// Waits for the open process `process` to end, under the hang guard.
    #[allow(unsafe_code)]
    fn wait_gone(process: &impl AsRawHandle) {
        let ms = u32::try_from(HANG_GUARD.as_millis()).expect("the guard fits");
        // SAFETY: the caller keeps `process` open for the call.
        let waited = unsafe { WaitForSingleObject(process.as_raw_handle(), ms) };
        assert_eq!(waited, WAIT_OBJECT_0, "the process did not end");
    }

    /// The suspend count of each thread of process `pid`, each left as it was.
    #[allow(unsafe_code)]
    fn suspend_counts(pid: u32) -> Vec<u32> {
        let mut counts = Vec::new();
        for_each_thread(pid, |thread| {
            // SAFETY: `thread` is open with THREAD_SUSPEND_RESUME for the call.
            let before = unsafe { SuspendThread(thread.as_raw_handle()) };
            assert_ne!(before, u32::MAX, "{}", io::Error::last_os_error());
            // SAFETY: as above; this undoes the suspend just made.
            let resumed = unsafe { ResumeThread(thread.as_raw_handle()) };
            assert_ne!(resumed, u32::MAX, "{}", io::Error::last_os_error());
            counts.push(before);
            Ok(())
        })
        .expect("list the threads");
        counts
    }

    /// The handles this process holds.
    #[allow(unsafe_code)]
    fn handle_count() -> u32 {
        let mut count = 0;
        // SAFETY: the pseudo handle needs no closing, and `count` outlives the call.
        let done = unsafe { GetProcessHandleCount(GetCurrentProcess(), &raw mut count) };
        assert_ne!(done, 0, "{}", io::Error::last_os_error());
        count
    }

    /// Gives `cmd` three standard handles of files of its own, which it holds until it is dropped.
    fn with_nul_files(cmd: &mut Command) {
        let nul = || {
            File::options()
                .read(true)
                .write(true)
                .open("NUL")
                .expect("open NUL")
        };
        cmd.stdin(nul()).stdout(nul()).stderr(nul());
    }

    /// Every way a start can fail, as often as it takes to see a handle stay behind. It runs in
    /// a process of its own, so that no other test's handles are in the count.
    fn failed_starts_keep_nothing() {
        const NAME: &str = "wx23a_spawn_failure_keeps_no_handles";
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("ran");
        let marked = || {
            let mut cmd = this_test(NAME, "mark");
            cmd.env(MARKER, &marker);
            with_nul_files(&mut cmd);
            cmd
        };
        // The control: a child that does run leaves the marker.
        let mut ran = spawn_in_job(&mut marked()).expect("a start");
        let status = ran.child_mut().wait().expect("a wait");
        assert!(status.success(), "{status:?}");
        std::fs::remove_file(&marker).expect("the control ran");
        drop(ran);

        let no_such_program = || {
            let mut cmd = Command::new(dir.path().join("no-such-program.exe"));
            with_nul_files(&mut cmd);
            let err = spawn_in_job(&mut cmd).expect_err("no such program");
            assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
        };
        // The refusal is the start's result, and the child that never ran is gone.
        let refused_at = |refuse: Stage| {
            let mut child_pid = None;
            let err = start_in_job(&mut marked(), &mut |stage, child, _job| {
                child_pid = Some(child.id());
                if stage == refuse {
                    return Err(io::Error::from(io::ErrorKind::ConnectionAborted));
                }
                Ok(())
            })
            .expect_err("a refused start");
            assert_eq!(err.kind(), io::ErrorKind::ConnectionAborted, "{err}");
            let pid = child_pid.expect("the child was made");
            assert!(process_exited(pid), "the refused child {pid} is gone");
        };
        let every_failure = || {
            no_such_program();
            refused_at(Stage::BeforeAssign);
            refused_at(Stage::BeforeResume);
        };
        // The first round lets the system load what it keeps for the whole process.
        every_failure();
        let before = handle_count();
        for _ in 0..5 {
            every_failure();
        }
        assert_eq!(handle_count(), before, "a failed start kept a handle");
        assert!(!marker.exists(), "a refused child never ran");
    }

    #[test]
    fn wx23a_spawn_failure_keeps_no_handles() {
        const NAME: &str = "wx23a_spawn_failure_keeps_no_handles";
        match mode().as_deref() {
            Some("count") => return failed_starts_keep_nothing(),
            Some("mark") => {
                let marker = std::env::var_os(MARKER).expect("the marker's path");
                std::fs::write(marker, b"ran").expect("write the marker");
                return;
            }
            _ => {}
        }
        // Its streams go to files: a process that a failing count leaves behind inherits them,
        // and would hold a pipe open.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("log");
        let file = File::create_new(&log).expect("create the log");
        let status = this_test(NAME, "count")
            .stdout(file.try_clone().expect("a second handle"))
            .stderr(file)
            .status()
            .expect("run the count");
        assert!(
            status.success(),
            "{}",
            std::fs::read_to_string(&log).expect("read the log")
        );
    }

    #[test]
    fn wx23a_child_enters_job_before_spawning_descendants() {
        const NAME: &str = "wx23a_child_enters_job_before_spawning_descendants";
        match mode().as_deref() {
            Some("leaf") => stay(),
            Some("parent") => {
                let leaf = this_test(NAME, "leaf").spawn().map(|child| child.id());
                say(&format!("wx23a-leaf {}", leaf.expect("a descendant")));
                let refused = this_test(NAME, "leaf")
                    .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
                    .spawn()
                    .map(|child| child.id())
                    .map_err(|e| e.raw_os_error());
                say(&format!("wx23a-breakaway {refused:?}"));
                stay();
            }
            _ => {}
        }
        let mut cmd = this_test(NAME, "parent");
        cmd.stdout(Stdio::piped());
        let mut stages = Vec::new();
        // Until the job holds it, the child has run nothing, so it can have started nothing.
        let mut started = start_in_job(&mut cmd, &mut |stage, child, job| {
            let counts = suspend_counts(child.id());
            assert!(!counts.is_empty(), "the child has a thread");
            assert!(counts.iter().all(|&n| n >= 1), "{stage:?}: {counts:?}");
            let held = stage == Stage::BeforeResume;
            assert_eq!(job.holds(child).expect("ask the job"), held, "{stage:?}");
            assert_eq!(
                job.active_processes().expect("ask the job"),
                u32::from(held),
                "{stage:?}"
            );
            stages.push(stage);
            Ok(())
        })
        .expect("a start");
        assert_eq!(stages, [Stage::BeforeAssign, Stage::BeforeResume]);

        // Once it runs, what it starts is in the job, and cannot leave it.
        let stdout = started.child_mut().stdout.take().expect("stdout");
        let said = Said::of(stdout);
        let leaf = said.leaf();
        assert_eq!(
            said.next(),
            format!(
                "wx23a-breakaway {:?}",
                Err::<u32, _>(Some(ERROR_ACCESS_DENIED as i32))
            ),
            "a job gives no breakaway"
        );
        assert!(started.job().holds(&leaf).expect("ask the job"));
        assert_eq!(started.job().active_processes().expect("ask the job"), 2);

        // The job's owner owns the tree. A child holding a handle of the job would keep it open
        // past this drop, and the tree would stay.
        let direct = open_process(started.id(), PROCESS_SYNCHRONIZE).expect("open the child");
        drop(started);
        wait_gone(&direct);
        wait_gone(&leaf);
    }

    #[test]
    fn wx23a_a_nested_job_stays_inside_the_outer_job() {
        const NAME: &str = "wx23a_a_nested_job_stays_inside_the_outer_job";
        match mode().as_deref() {
            Some("leaf") => stay(),
            Some("middle") => {
                let mut inner = spawn_in_job(&mut this_test(NAME, "leaf")).expect("a nested start");
                say(&format!("wx23a-leaf {}", inner.id()));
                let mut line = String::new();
                std::io::stdin().read_line(&mut line).expect("the word");
                inner.job().terminate().expect("end the inner job");
                let status = inner.child_mut().wait().expect("a wait");
                assert_eq!(status.code(), Some(KILLED_EXIT_CODE as i32));
                say("wx23a-inner-ended");
                let second = spawn_in_job(&mut this_test(NAME, "leaf")).expect("another start");
                say(&format!("wx23a-leaf {}", second.id()));
                stay();
            }
            _ => {}
        }
        let mut cmd = this_test(NAME, "middle");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut outer = spawn_in_job(&mut cmd).expect("a start");
        let mut stdin = outer.child_mut().stdin.take().expect("stdin");
        let stdout = outer.child_mut().stdout.take().expect("stdout");
        let said = Said::of(stdout);

        // The child of a process that is in a job gets a job of its own, and stays in the outer.
        let first = said.leaf();
        assert!(outer.job().holds(&first).expect("ask the job"));
        assert_eq!(outer.job().active_processes().expect("ask the job"), 2);

        // Ending the inner job ends only what it holds.
        stdin.write_all(b"end the inner job\n").expect("the word");
        assert_eq!(said.next(), "wx23a-inner-ended");
        wait_gone(&first);
        assert!(process_running(outer.id()), "the middle process stays");

        // Ending the outer job ends what the inner jobs hold too.
        let second = said.leaf();
        assert!(outer.job().holds(&second).expect("ask the job"));
        outer.job().terminate().expect("end the outer job");
        wait_gone(&second);
        let status = outer.child_mut().wait().expect("a wait");
        assert_eq!(status.code(), Some(KILLED_EXIT_CODE as i32));
        assert_eq!(outer.job().active_processes().expect("ask the job"), 0);
        // A job with nothing left to end is already stopped.
        outer.job().terminate().expect("end an empty job");
    }

    /// A suspended child looks up its start directory only when it runs, so the pin has to hold
    /// through the whole start, as `start_in_dir` asks of its callers.
    #[test]
    fn wx23a_a_suspended_start_keeps_its_directory_pinned() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("p");
        let moved = root.path().join("moved");
        std::fs::create_dir(&path).expect("mkdir");
        std::fs::write(path.join("marker"), b"mine").expect("write the marker");
        std::fs::write(path.join("show.cmd"), b"@type marker\r\n").expect("write the program");
        let dir = rdy_store::pio::open_dir(&path).expect("open the directory");
        let program = program_in_dir(&dir, "show.cmd".as_ref()).expect("the program's path");
        let mut cmd = Command::new(program.path());
        let start = start_in_dir(&mut cmd, &dir).expect("the start directory");
        cmd.stdin(Stdio::null()).stdout(Stdio::piped());
        let started = start_in_job(&mut cmd, &mut |stage, _child, _job| {
            if stage == Stage::BeforeResume {
                let err = std::fs::rename(&path, &moved).expect_err("a pinned directory stays");
                let code = err.raw_os_error();
                assert!(matches!(code, Some(32 | 5)), "{err}");
            }
            Ok(())
        })
        .expect("a start");
        let (child, _job) = started.into_parts();
        let out = child.wait_with_output().expect("a wait");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(out.stdout, b"mine");
        drop((start, program, dir));
        std::fs::rename(&path, &moved).expect("no pin holds it now");
    }

    #[test]
    fn wx23a_a_child_runs_then_exits_after_kill_hard() {
        const NAME: &str = "wx23a_a_child_runs_then_exits_after_kill_hard";
        if mode().is_some() {
            stay();
        }
        let mut child = this_test(NAME, "leaf").spawn().expect("a start");
        let pid = child.id();
        assert!(process_running(pid), "the child is running");
        assert!(!process_exited(pid), "the child has not exited");
        kill_hard(pid);
        let status = child.wait().expect("reap the child");
        assert_eq!(status.code(), Some(KILLED_EXIT_CODE as i32), "{status:?}");
        // This process still holds the child's handle, as a zombie's parent would.
        assert!(process_exited(pid), "an ended child with an open handle");
        assert!(!process_running(pid));
        kill_hard(pid);
        drop(child);
        assert!(process_exited(pid), "no process has this id");
        assert!(!process_running(pid));
    }

    /// Where a part says that it is ready: a file that appears whole, never half written.
    const READY: &str = "RDY_WX23B_READY";
    /// The process id of the test that started a part.
    const PARENT: &str = "RDY_WX23B_PARENT";

    /// Calls `check` until it holds, under the hang guard. The pause only spaces the calls out.
    fn eventually(what: &str, mut check: impl FnMut() -> bool) {
        for _ in 0..(HANG_GUARD.as_millis() / 10) {
            if check() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("never happened: {what}");
    }

    /// Runs `work` on a thread of its own and returns its result; a run that hangs fails the test.
    fn within_the_guard<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        rx.recv_timeout(HANG_GUARD)
            .expect("the bounded run hung past the guard")
    }

    /// Says to the test that started this part that it is ready, with `text`.
    fn ready(text: &str) {
        let path = std::path::PathBuf::from(std::env::var_os(READY).expect("the ready path"));
        let part = path.with_extension("part");
        std::fs::write(&part, text).expect("write the ready text");
        std::fs::rename(&part, &path).expect("publish the ready text");
    }

    /// The text a part said with [`ready`] at `path`.
    fn ready_text(path: &std::path::Path) -> String {
        eventually("a part said it is ready", || path.exists());
        std::fs::read_to_string(path).expect("read the ready text")
    }

    /// A part that starts a descendant sharing its streams and says which process it is. The
    /// descendant belongs to the job, which is what ends it.
    #[allow(clippy::zombie_processes)]
    fn start_descendant(name: &str) {
        let mut cmd = this_test(name, "leaf");
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        let leaf = cmd.spawn().expect("a descendant");
        ready(&leaf.id().to_string());
    }

    /// The test's `cmd.exe` that prints `out` to stdout and `err` to stderr, then exits `code`.
    fn cmd_script(script: &str) -> Command {
        let mut cmd = Command::new("cmd");
        cmd.raw_arg(format!("/D /C \"{script}\""));
        cmd
    }

    #[test]
    fn wx23b_runner_caps_both_streams_and_reaps_the_tree() {
        const NAME: &str = "wx23b_runner_caps_both_streams_and_reaps_the_tree";
        match mode().as_deref() {
            Some("leaf") => stay(),
            Some("flood") => {
                start_descendant(NAME);
                // 1200 bytes against a cap of 1000, and then it runs on: only the cap ends it.
                let mut out = std::io::stdout();
                out.write_all(&[b'o'; 600]).expect("write stdout");
                out.flush().expect("flush stdout");
                std::io::stderr()
                    .write_all(&[b'e'; 600])
                    .expect("write stderr");
                stay();
            }
            Some("orphan") => {
                // The child ends at once and leaves a descendant that holds both streams.
                start_descendant(NAME);
                return;
            }
            _ => {}
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let ready_path = dir.path().join("ready");
        let part = |mode: &str| {
            let mut cmd = this_test(NAME, mode);
            cmd.env(READY, &ready_path);
            cmd
        };
        let gone = |ready_path: &std::path::Path| {
            let pid: u32 = ready_text(ready_path).trim().parse().expect("a pid");
            eventually("the descendant ended", || process_exited(pid));
        };

        // Both streams share one cap; the tree ends with the run, and the child is reaped.
        let cmd = part("flood");
        let run = within_the_guard(move || run_bounded_inner(cmd, None, None, 1000, None, drop));
        let err = run.result.expect_err("1200 bytes pass a cap of 1000");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        assert_eq!(run.retained, 1000, "the bytes kept stop at the cap");
        assert_eq!(run.drains_joined, 2, "both drains were joined");
        assert!(
            process_exited(run.pid.expect("a start")),
            "the child was reaped"
        );
        gone(&ready_path);

        // A descendant that holds the streams after its parent exits leaves no drain behind.
        std::fs::remove_file(&ready_path).expect("clear the ready file");
        let cmd = part("orphan");
        let run = within_the_guard(move || run_bounded_inner(cmd, None, None, 1 << 20, None, drop));
        let out = run.result.expect("the child exits 0");
        assert!(out.status.success(), "{:?}", out.status);
        assert_eq!(run.drains_joined, 2);
        gone(&ready_path);

        // At the cap both streams are kept whole, and a nonzero status is an Output.
        let out = within_the_guard(|| {
            run_bounded(
                cmd_script("echo out& echo err>&2& exit 3"),
                None,
                10,
                Duration::from_secs(600),
            )
        })
        .expect("10 bytes fit a cap of 10");
        assert_eq!(
            (out.stdout.as_slice(), out.stderr.as_slice()),
            (&b"out\r\n"[..], &b"err\r\n"[..])
        );
        assert_eq!(out.status.code(), Some(3));
        // One byte over, split across the streams.
        let err = within_the_guard(|| {
            run_bounded(
                cmd_script("echo out& echo err>&2"),
                None,
                9,
                Duration::from_secs(600),
            )
        })
        .expect_err("10 bytes pass a cap of 9");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");

        // stdout can go to a file the caller opened; only stderr counts against the cap.
        let target = dir.path().join("archive");
        let file = File::create_new(&target).expect("create");
        let out = within_the_guard(move || {
            run_bounded_to(
                cmd_script("echo archive bytes over the cap& echo err>&2"),
                None,
                file,
                5,
                Duration::from_secs(600),
            )
        })
        .expect("stderr fits a cap of 5");
        assert!(out.stdout.is_empty(), "stdout went to the file");
        assert_eq!(out.stderr, b"err\r\n");
        assert_eq!(
            std::fs::read(&target).expect("read"),
            b"archive bytes over the cap\r\n"
        );
        let file = File::create_new(dir.path().join("second")).expect("create");
        let run = within_the_guard(move || {
            run_bounded_inner(cmd_script("echo errs>&2"), None, Some(file), 3, None, drop)
        });
        let err = run.result.expect_err("6 bytes pass a cap of 3");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        assert_eq!(run.drains_joined, 1, "one drain, for stderr");
        assert_eq!(run.retained, 3);

        // stdin comes from the given file; a child that cannot start keeps its error kind.
        let input = dir.path().join("input");
        std::fs::write(&input, b"line\r\n").expect("write the input");
        let file = File::open(&input).expect("open the input");
        let out = within_the_guard(move || {
            let mut cmd = Command::new("findstr");
            cmd.arg("^");
            run_bounded(cmd, Some(file), 100, Duration::from_secs(600))
        })
        .expect("read stdin");
        assert_eq!(out.stdout, b"line\r\n");
        let before = runner_starts();
        let err = run_bounded(
            Command::new(dir.path().join("no-such-program.exe")),
            None,
            100,
            Duration::from_secs(600),
        )
        .expect_err("no such program");
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
        assert_eq!(runner_starts(), before + 1);
    }

    #[test]
    fn wx23b_cancel_wakes_blocked_output_drains() {
        let (tx, rx) = mpsc::channel();
        let count = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(Signal::new().expect("a signal"));
        let (read, write) = PipeRead::pair().expect("a pipe");
        let drain = spawn_drain(read, &count, &cancel, 1000, &tx).expect("a drain");
        // The writer stays open and silent after three bytes, so only the cancel can end the read.
        let mut writer = File::from(write);
        writer.write_all(b"abc").expect("write");
        eventually("the drain counted the bytes", || {
            count.load(Ordering::SeqCst) == 3
        });
        cancel.set();
        let kept = within_the_guard(move || drain.join().expect("the drain ran"));
        assert_eq!(kept, b"abc", "what arrived before the cancel is kept");
        assert!(matches!(rx.try_recv(), Ok(RunEvent::DrainDone)));
        assert!(rx.try_recv().is_err(), "the drain reported once");
        drop(writer);

        // A cancel that is already set stops a drain before it reads.
        let (tx, rx) = mpsc::channel();
        let (read, write) = PipeRead::pair().expect("a pipe");
        let drain = spawn_drain(read, &count, &cancel, 1000, &tx).expect("a drain");
        let kept = within_the_guard(move || drain.join().expect("the drain ran"));
        assert!(kept.is_empty());
        assert!(matches!(rx.recv(), Ok(RunEvent::DrainDone)));
        drop(write);

        // Overflow stops a drain on its own, and the bytes kept stop at the cap.
        let (tx, rx) = mpsc::channel();
        let count = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(Signal::new().expect("a signal"));
        let (read, write) = PipeRead::pair().expect("a pipe");
        let drain = spawn_drain(read, &count, &cancel, 4, &tx).expect("a drain");
        let mut writer = File::from(write);
        writer.write_all(b"abcdefgh").expect("write");
        let kept = within_the_guard(move || drain.join().expect("the drain ran"));
        assert_eq!(kept, b"abcd");
        assert!(matches!(rx.recv(), Ok(RunEvent::Overflow)));
        drop(writer);
    }

    #[test]
    #[allow(unsafe_code)]
    fn wx23b_escaped_pipe_holder_cannot_hang_the_gate() {
        const NAME: &str = "wx23b_escaped_pipe_holder_cannot_hang_the_gate";
        if mode().as_deref() == Some("thief") {
            // Hands copies of both streams to the process that started this part, which is
            // outside the job, and ends. Then only that process holds the pipes.
            let parent: u32 = std::env::var(PARENT)
                .expect("the parent")
                .parse()
                .expect("a pid");
            let target = open_process(parent, PROCESS_DUP_HANDLE).expect("open the parent");
            let steal = |which| {
                let mut stolen: HANDLE = null_mut();
                // SAFETY: the standard handle and the parent are open for the call, and
                // `stolen` outlives it.
                let done = unsafe {
                    DuplicateHandle(
                        GetCurrentProcess(),
                        GetStdHandle(which),
                        target.as_raw_handle(),
                        &raw mut stolen,
                        0,
                        0,
                        DUPLICATE_SAME_ACCESS,
                    )
                };
                assert_ne!(done, 0, "{}", io::Error::last_os_error());
                stolen as usize
            };
            // SAFETY: the call takes no pointer.
            let me = unsafe { GetCurrentProcessId() };
            ready(&format!(
                "{me} {} {}",
                steal(STD_OUTPUT_HANDLE),
                steal(STD_ERROR_HANDLE)
            ));
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let ready_path = dir.path().join("ready");
        let mut cmd = this_test(NAME, "thief");
        // SAFETY: the call takes no pointer.
        cmd.env(READY, &ready_path)
            .env(PARENT, unsafe { GetCurrentProcessId() }.to_string());
        // The run has no deadline of its own: only the injected expiry can end it, and it comes
        // after the child ended, when only the stolen copies hold the streams.
        let (held_tx, held) = mpsc::channel();
        let hook_ready = ready_path.clone();
        let run = within_the_guard(move || {
            run_bounded_inner(cmd, None, None, 1 << 20, None, move |expiry| {
                std::thread::spawn(move || {
                    let text = ready_text(&hook_ready);
                    let [child, out, err] = text
                        .split_whitespace()
                        .map(|n| n.parse::<usize>().expect("a number"))
                        .collect::<Vec<_>>()[..]
                    else {
                        panic!("the part said {text:?}");
                    };
                    // SAFETY: the child made these two handles in this process for it, and
                    // nothing else owns them.
                    let stolen = unsafe {
                        [out, err].map(|h| OwnedHandle::from_raw_handle(h as *mut c_void))
                    };
                    let child = u32::try_from(child).expect("a pid");
                    eventually("the direct child exited", || process_exited(child));
                    expiry.expire();
                    let _ = held_tx.send(stolen);
                });
            })
        });
        let stolen = held.recv_timeout(HANG_GUARD).expect("the stolen handles");
        let err = run.result.expect_err("the injected expiry ends the run");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert_eq!(run.drains_joined, 2, "both drains were joined");
        assert!(
            process_exited(run.pid.expect("a start")),
            "the child was reaped"
        );
        drop(stolen);
    }
}
