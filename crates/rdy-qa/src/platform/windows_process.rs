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
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, OpenProcess, OpenThread, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    ResumeThread, THREAD_SUSPEND_RESUME, TerminateProcess, WaitForSingleObject,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::process::CommandExt;
    use std::process::{ChildStdout, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, GetCurrentProcess, GetProcessHandleCount,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, ResumeThread, SuspendThread,
        WaitForSingleObject,
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
}
