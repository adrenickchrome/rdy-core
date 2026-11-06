//! The stop of a daemon of another version, through the `sys` seam. The proof chain is pure, so a
//! scripted system runs it on every platform and no test waits out a grace period. What a real
//! event, a real process and a real handle do runs natively, as far as each platform has it. The
//! Windows crate does not build until every `sys` part exists, so an isolated probe runs this file
//! there until then.

use rdy_daemon::sys::records::{
    PIPE_PREFIX, STOP_EVENT_PREFIX, hash_of_units, pipe_name, stop_event_name, stop_event_of_pipe,
};
use rdy_daemon::sys::stop_proof::{Refusal, StopBoundary, StopOutcome, stop_refused_version};
use rdy_daemon::sys::{self, Liveness};
use rdy_daemon::{
    E_DAEMON_VERSION, ErrorReply, Hello, HelloReply, PROTO_VERSION, Proto, RunDir, read_hello,
    request_stop, socket,
};
use rdy_store::fsutil::write_private_durable;
use std::cell::{Cell, RefCell};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const PID: u32 = 4242;
const BORN: u64 = 1_000;
const GRACE: Duration = Duration::from_secs(5);
/// ERROR_ACCESS_DENIED: any error will do, and this one is a real answer.
const DENIED: i32 = 5;
/// A hung test fails here. It asserts no timing: every wait below ends on an event.
const HANG_LIMIT: Duration = Duration::from_secs(120);

fn pipe() -> PathBuf {
    pipe_name(&hash_of_units("C:\\p".encode_utf16()))
}

fn other_pipe() -> PathBuf {
    pipe_name(&hash_of_units("C:\\other".encode_utf16()))
}

fn refusal() -> Refusal {
    Refusal {
        endpoint: pipe(),
        pid: Some(PID),
        created: BORN,
    }
}

/// What the run files and the retained handle say at one moment.
#[derive(Clone)]
struct World {
    record: Option<PathBuf>,
    pid_file: Option<(u32, u64)>,
    held: Result<u64, i32>,
    exited: Result<bool, i32>,
}

fn proved() -> World {
    World {
        record: Some(pipe()),
        pid_file: Some((PID, BORN)),
        held: Ok(BORN),
        exited: Ok(false),
    }
}

/// A scripted system. It logs what a stop does to the daemon; reading changes nothing.
struct Script {
    before_signal: World,
    after_signal: World,
    signal: Result<(), i32>,
    /// What the wait says: whether the process ended within the grace period.
    wait: Result<bool, i32>,
    terminate: Result<(), i32>,
    /// A failed termination finds the process had ended.
    exited_at_termination: bool,
    signalled: Cell<bool>,
    terminating: Cell<bool>,
    log: RefCell<Vec<String>>,
}

impl Script {
    fn new() -> Script {
        Script {
            before_signal: proved(),
            after_signal: proved(),
            signal: Ok(()),
            wait: Ok(false),
            terminate: Ok(()),
            exited_at_termination: false,
            signalled: Cell::new(false),
            terminating: Cell::new(false),
            log: RefCell::new(Vec::new()),
        }
    }

    fn world(&self) -> &World {
        if self.signalled.get() {
            &self.after_signal
        } else {
            &self.before_signal
        }
    }

    fn did(&self, what: impl Into<String>) {
        self.log.borrow_mut().push(what.into());
    }

    fn did_nothing(&self) -> bool {
        self.log.borrow().is_empty()
    }
}

impl StopBoundary for Script {
    fn record(&self) -> Option<PathBuf> {
        self.world().record.clone()
    }

    fn pid_file(&self) -> Option<(u32, u64)> {
        self.world().pid_file
    }

    fn held_created(&self) -> io::Result<u64> {
        self.world().held.map_err(io::Error::from_raw_os_error)
    }

    fn has_exited(&self) -> io::Result<bool> {
        if self.terminating.get() {
            return Ok(self.exited_at_termination);
        }
        self.world().exited.map_err(io::Error::from_raw_os_error)
    }

    fn signal(&self) -> io::Result<()> {
        self.did("signal");
        self.signalled.set(true);
        self.signal.map_err(io::Error::from_raw_os_error)
    }

    fn wait_exit(&self, grace: Duration) -> io::Result<bool> {
        self.did(format!("wait {} s", grace.as_secs()));
        self.wait.map_err(io::Error::from_raw_os_error)
    }

    fn terminate(&self) -> io::Result<()> {
        self.did("terminate");
        self.terminating.set(true);
        self.terminate.map_err(io::Error::from_raw_os_error)
    }
}

/// An outcome as a text, with the error's number: the system's words differ by platform.
fn shape(outcome: &StopOutcome) -> String {
    match outcome {
        StopOutcome::Untied => "untied".into(),
        StopOutcome::Exited { pid } => format!("exited {pid}"),
        StopOutcome::Signalled { pid } => format!("signalled {pid}"),
        StopOutcome::Stopped { pid } => format!("stopped {pid}"),
        StopOutcome::Draining { pid } => format!("draining {pid}"),
        StopOutcome::Terminated { pid } => format!("terminated {pid}"),
        StopOutcome::SignalFailed { pid, error } => {
            format!("signal failed {pid} {:?}", error.raw_os_error())
        }
        StopOutcome::TerminateFailed { pid, error } => {
            format!("terminate failed {pid} {:?}", error.raw_os_error())
        }
    }
}

fn stop(script: &Script) -> String {
    shape(&stop_refused_version(script, &refusal(), GRACE))
}

/// The states of the record, the pid file and the retained handle, each with whether it proves.
fn proof_states() -> Vec<(bool, World)> {
    let mut states = Vec::new();
    for (record_ok, record) in [
        (true, Some(pipe())),
        (false, None),
        (false, Some(other_pipe())),
    ] {
        for (pid_file_ok, pid_file) in [
            (true, Some((PID, BORN))),
            (false, None),
            (false, Some((PID + 1, BORN))),
            (false, Some((PID, BORN + 1))),
        ] {
            for (held_ok, held) in [
                (true, Ok(BORN)),
                (false, Ok(BORN + 1)),
                (false, Err(DENIED)),
            ] {
                states.push((
                    record_ok && pid_file_ok && held_ok,
                    World {
                        record: record.clone(),
                        pid_file,
                        held,
                        exited: Ok(false),
                    },
                ));
            }
        }
    }
    states
}

#[test]
fn wx16_force_stop_requires_all_identity_proofs() {
    // before anything is sent: only every proof together lets the signal go
    let mut seen = 0;
    for (all, world) in proof_states() {
        let script = Script {
            before_signal: world.clone(),
            ..Script::new()
        };
        let got = stop(&script);
        if all {
            seen += 1;
            assert_eq!(got, format!("terminated {PID}"));
            assert_eq!(*script.log.borrow(), ["signal", "wait 5 s", "terminate"]);
        } else {
            assert_eq!(
                got, "untied",
                "{:?} {:?} {:?}",
                world.record, world.pid_file, world.held
            );
            assert!(script.did_nothing(), "{:?}", script.log.borrow());
        }
    }
    assert_eq!(seen, 1);

    // when the grace period ends: the signal went, and only every proof together lets the force go
    let mut seen = 0;
    for (all, world) in proof_states() {
        let script = Script {
            after_signal: world.clone(),
            ..Script::new()
        };
        let got = stop(&script);
        let both_files_gone = world.record.is_none() && world.pid_file.is_none();
        if all {
            seen += 1;
            assert_eq!(got, format!("terminated {PID}"));
        } else if both_files_gone {
            // the daemon's own removal: it heard the stop and drains. Nothing forces it
            assert_eq!(got, format!("draining {PID}"));
            assert_eq!(*script.log.borrow(), ["signal", "wait 5 s"]);
        } else {
            assert_eq!(
                got, "untied",
                "{:?} {:?} {:?}",
                world.record, world.pid_file, world.held
            );
            assert_eq!(*script.log.borrow(), ["signal", "wait 5 s"]);
        }
    }
    assert_eq!(seen, 1);

    // the connection gave no pid, or one that is no process of ours: nothing is sent, whatever the
    // pid file says
    for pid in [None, Some(0), Some(1)] {
        let script = Script {
            before_signal: World {
                pid_file: Some((pid.unwrap_or(PID), BORN)),
                ..proved()
            },
            ..Script::new()
        };
        let refused = Refusal { pid, ..refusal() };
        assert_eq!(
            shape(&stop_refused_version(&script, &refused, GRACE)),
            "untied"
        );
        assert!(script.did_nothing());
    }
}

#[test]
fn wx16_pid_reuse_never_kills_the_replacement() {
    // the daemon left during the grace period, and a replacement has its pid: nothing is forced
    let replaced = World {
        pid_file: Some((PID, BORN + 500)),
        exited: Ok(true),
        ..proved()
    };
    let left = Script {
        after_signal: replaced.clone(),
        ..Script::new()
    };
    assert_eq!(stop(&left), format!("stopped {PID}"));
    assert_eq!(*left.log.borrow(), ["signal", "wait 5 s"]);

    // the wait itself saw the end
    let ended = Script {
        wait: Ok(true),
        after_signal: replaced,
        ..Script::new()
    };
    assert_eq!(stop(&ended), format!("stopped {PID}"));
    assert_eq!(*ended.log.borrow(), ["signal", "wait 5 s"]);

    // a replacement's pid file, while the retained process still runs: the proof has changed
    let moved_on = Script {
        after_signal: World {
            pid_file: Some((PID, BORN + 500)),
            ..proved()
        },
        ..Script::new()
    };
    assert_eq!(stop(&moved_on), "untied");
    assert_eq!(*moved_on.log.borrow(), ["signal", "wait 5 s"]);

    // the matching pid with another creation time, before anything is sent
    let wrong_start = Script {
        before_signal: World {
            pid_file: Some((PID, BORN + 500)),
            ..proved()
        },
        ..Script::new()
    };
    assert_eq!(stop(&wrong_start), "untied");
    assert!(wrong_start.did_nothing());

    // the handle holds another process than the refused connection's
    let other_held = Script {
        before_signal: World {
            held: Ok(BORN + 500),
            ..proved()
        },
        ..Script::new()
    };
    assert_eq!(stop(&other_held), "untied");
    assert!(other_held.did_nothing());
}

#[test]
fn wx16_grace_completion_forces_nothing() {
    let script = Script {
        wait: Ok(true),
        ..Script::new()
    };
    assert_eq!(stop(&script), format!("stopped {PID}"));
    assert_eq!(*script.log.borrow(), ["signal", "wait 5 s"]);
}

#[test]
fn wx16_an_exited_daemon_needs_nothing() {
    // before the proofs: its run files may already be gone, and that ties nothing to anything
    let script = Script {
        before_signal: World {
            record: None,
            pid_file: None,
            exited: Ok(true),
            ..proved()
        },
        ..Script::new()
    };
    assert_eq!(stop(&script), format!("exited {PID}"));
    assert!(script.did_nothing());
}

#[test]
fn wx16_a_handle_that_cannot_answer_proves_nothing() {
    let before = Script {
        before_signal: World {
            exited: Err(DENIED),
            ..proved()
        },
        ..Script::new()
    };
    assert_eq!(stop(&before), "untied");
    assert!(before.did_nothing());

    let after = Script {
        after_signal: World {
            exited: Err(DENIED),
            ..proved()
        },
        ..Script::new()
    };
    assert_eq!(stop(&after), "untied");
    assert_eq!(*after.log.borrow(), ["signal", "wait 5 s"]);

    let wait = Script {
        wait: Err(DENIED),
        ..Script::new()
    };
    assert_eq!(stop(&wait), "untied");
    assert_eq!(*wait.log.borrow(), ["signal", "wait 5 s"]);
}

#[test]
fn wx16_a_failed_signal_is_its_own_failure_and_forces_nothing() {
    let script = Script {
        signal: Err(DENIED),
        ..Script::new()
    };
    assert_eq!(stop(&script), format!("signal failed {PID} Some({DENIED})"));
    assert_eq!(*script.log.borrow(), ["signal"]);

    // no event to signal because the daemon had just left
    let left = Script {
        signal: Err(DENIED),
        after_signal: World {
            exited: Ok(true),
            ..proved()
        },
        ..Script::new()
    };
    assert_eq!(stop(&left), format!("exited {PID}"));
}

#[test]
fn wx16_a_failed_termination_is_its_own_failure() {
    let script = Script {
        terminate: Err(DENIED),
        ..Script::new()
    };
    assert_eq!(
        stop(&script),
        format!("terminate failed {PID} Some({DENIED})")
    );

    // a process that ended in that instant refuses too: it has ended, which is what was asked
    let raced = Script {
        terminate: Err(DENIED),
        exited_at_termination: true,
        ..Script::new()
    };
    assert_eq!(stop(&raced), format!("stopped {PID}"));
}

#[test]
fn wx16_the_event_has_the_pipes_hash() {
    let hash = hash_of_units("C:\\p".encode_utf16());
    let name = stop_event_name(&hash);
    assert_eq!(name, format!(r"Local\rdy-stop-{hash}"));
    assert!(name.starts_with(STOP_EVENT_PREFIX));
    assert_eq!(stop_event_of_pipe(&pipe_name(&hash)), Some(name));

    // nothing but a whole rdy pipe name has an event
    let digits = "a".repeat(64);
    for bad in [
        String::new(),
        PIPE_PREFIX.to_string(),
        format!("{PIPE_PREFIX}{}", "a".repeat(63)),
        format!("{PIPE_PREFIX}{}", "A".repeat(64)),
        format!("{PIPE_PREFIX}{digits}x"),
        format!(r"\\host\pipe\rdy-{digits}"),
        format!(r"\\.\pipe\other-{digits}"),
    ] {
        assert_eq!(stop_event_of_pipe(Path::new(&bad)), None, "{bad}");
    }
}

// Real processes. The test binary runs itself as each of them.

/// What this binary is when a test starts it, and the variables that tell it the rest.
const ROLE: &str = "RDY_WX16_ROLE";
const ROOT: &str = "RDY_WX16_ROOT";

/// A new project directory with its `.rdy`. On Unix under /tmp, so `.rdy/run/rdy.sock` fits.
fn project() -> tempfile::TempDir {
    let dir = if cfg!(windows) {
        tempfile::tempdir().unwrap()
    } else {
        tempfile::Builder::new()
            .prefix("rdy-wx16")
            .tempdir_in("/tmp")
            .unwrap()
    };
    std::fs::create_dir(dir.path().join(".rdy")).unwrap();
    dir
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// This test binary, running only the test that holds the other roles.
fn this_binary(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["wx16_child_role", "--exact", "--nocapture"])
        .env(ROLE, role);
    command
}

/// A child that serves a project as a daemon would. It goes when this is dropped.
struct Daemon {
    child: Child,
    lines: Receiver<String>,
}

impl Daemon {
    fn start(root: &Path, role: &str) -> Daemon {
        let mut child = this_binary(role)
            .env(ROOT, root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in out.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let mut daemon = Daemon { child, lines };
        daemon.heard("ready");
        daemon
    }

    /// Waits for the line that says `words`.
    fn heard(&mut self, words: &str) {
        loop {
            let line = self
                .lines
                .recv_timeout(HANG_LIMIT)
                .unwrap_or_else(|e| panic!("no \"{words}\" from the child: {e}"));
            if line.contains(words) {
                return;
            }
        }
    }

    fn runs(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What a daemon of another version with other clients says to every hello. A connection that
/// sends none only holds its place until it goes.
async fn refuse_every_hello(mut listener: sys::Listener) {
    while let Ok(mut stream) = listener.accept().await {
        tokio::spawn(async move {
            if read_hello(&mut stream).await.is_err() {
                return;
            }
            let reply = HelloReply::Refused {
                error: ErrorReply::new(E_DAEMON_VERSION, "another version", "stop it"),
            };
            let mut line = serde_json::to_vec(&reply).unwrap();
            line.push(b'\n');
            let _ = stream.write_all(&line).await;
            let _ = stream.flush().await;
        });
    }
}

/// What the child roles have in common: the signals, then the project's endpoint and run files.
async fn serve_until(role: impl AsyncFnOnce(sys::StopSignals)) {
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    let run = RunDir::of_project(&root);
    let signals = sys::StopSignals::install(&run.root()).unwrap();
    let (listener, _, _) = socket::bind(&run).await.unwrap();
    tokio::spawn(refuse_every_hello(listener));
    println!("\nready");
    role(signals).await;
}

/// A client's hello, of the version that is refused.
fn hello(run: &RunDir) -> Hello {
    Hello {
        rdy: PROTO_VERSION,
        version: "0.2.0".into(),
        build: rdy_daemon::BUILD.into(),
        proto: Proto::Ctl,
        cwd: run.root(),
        project: run.project_id().unwrap(),
    }
}

/// The child's side of [`wx16_event_stops_a_refusing_version`].
fn heeds_the_stop() {
    runtime().block_on(serve_until(async |mut signals: sys::StopSignals| {
        let name = signals.recv().await;
        println!("\nstopped by {name}");
    }));
}

#[test]
fn wx16_child_role() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    match role.as_str() {
        "heeds" => heeds_the_stop(),
        #[cfg(windows)]
        other => native::child_role(other),
        #[cfg(not(windows))]
        other => panic!("no role {other}"),
    }
}

#[test]
fn wx16_event_stops_a_refusing_version() {
    let dir = project();
    let mut daemon = Daemon::start(dir.path(), "heeds");
    let run = RunDir::of_project(dir.path());
    let pid = daemon.child.id();
    let endpoint = run.recorded_socket().unwrap();
    let request = runtime()
        .block_on(request_stop(&run, &hello(&run), HANG_LIMIT))
        .unwrap()
        .expect("a daemon answers");
    assert_eq!(request.pid, Some(pid));
    assert_eq!(request.socket, endpoint);
    assert!(request.signalled);
    // it left by its own path: a forced end would neither say this nor exit with success
    daemon.heard(if cfg!(windows) {
        "stopped by the stop event"
    } else {
        "stopped by SIGTERM"
    });
    assert!(daemon.child.wait().unwrap().success());
}

#[test]
fn wx16_a_stop_with_a_forged_pid_file_is_the_untied_refusal() {
    let dir = project();
    let mut daemon = Daemon::start(dir.path(), "heeds");
    let run = RunDir::of_project(dir.path());
    // this process, by its whole identity: alive, of this account, and not the daemon
    write_private_durable(&run.pid_file(), &sys::own_pid_record().unwrap()).unwrap();
    let e = runtime()
        .block_on(request_stop(&run, &hello(&run), HANG_LIMIT))
        .unwrap_err();
    assert_eq!(e.code, E_DAEMON_VERSION);
    assert_eq!(
        e.message,
        "the running daemon is another rdy version and has other clients, and rdy cannot tie its process to this project"
    );
    assert_eq!(
        e.fix,
        "close the daemon's other sessions, then run rdy serve --stop again"
    );
    assert!(daemon.runs(), "the daemon was stopped");
}

#[test]
fn wx16_process_liveness_proves_only_an_end() {
    let mut child = this_binary("none")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_remove(ROLE)
        .spawn()
        .unwrap();
    let pid = child.id();
    drop(child.stdin.take());
    child.wait().unwrap();
    // the exit is reaped; nothing here holds the process
    drop(child);
    assert_eq!(sys::process_liveness(pid), Liveness::Gone);
    assert!(!rdy_daemon::process_alive(pid));

    let me = std::process::id();
    assert_eq!(sys::process_liveness(me), Liveness::Live);
    assert!(rdy_daemon::process_alive(me));
}

#[cfg(windows)]
mod native {
    use super::*;
    use rdy_daemon::sys::records::pipe_pid_record;
    use rdy_store::pio::{self, Ace, ObjectKind};
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, FALSE, HANDLE,
        WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::{
        ACL, ACL_REVISION, AddAccessAllowedAce, CreateRestrictedToken, DACL_SECURITY_INFORMATION,
        DuplicateToken, InitializeAcl, InitializeSecurityDescriptor, SECURITY_DESCRIPTOR,
        SID_AND_ATTRIBUTES, SecurityImpersonation, SetKernelObjectSecurity,
        SetSecurityDescriptorDacl, TOKEN_DUPLICATE, TOKEN_QUERY,
    };
    use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
    use windows_sys::Win32::System::Console::GetConsoleCP;
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CreateEventW, EVENT_ALL_ACCESS, EVENT_MODIFY_STATE, GetCurrentProcess,
        INFINITE, OpenEventW, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, SYNCHRONIZATION_SYNCHRONIZE, SetEvent,
        SetThreadToken, WaitForSingleObject,
    };

    /// The real proofs of a connection to the project's recorded endpoint, as a client holds them.
    async fn refused_connection(run: &RunDir) -> (PathBuf, sys::PeerProcess, sys::Stream) {
        let endpoint = run.recorded_socket().expect("a record for this project");
        let stream = sys::connect(&endpoint).await.unwrap();
        let process = sys::check_daemon(&stream, &endpoint, sys::own_account().unwrap()).unwrap();
        (endpoint, process, stream)
    }

    /// The variable that names the event a detached child waits on.
    const RELEASE: &str = "RDY_WX16_RELEASE";
    const LOCK: &str = "RDY_WX16_LOCK";
    const LOG: &str = "RDY_WX16_LOG";
    const AGENT: &str = "RDY_WX16_AGENT";

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(Some(0))
            .collect()
    }

    fn event_of(root: &Path) -> String {
        stop_event_name(&sys::project_hash(root))
    }

    #[allow(unsafe_code)]
    fn open_event(name: &str, access: u32) -> io::Result<OwnedHandle> {
        let name = wide(name);
        // SAFETY: `name` is a NUL-terminated string that outlives the call.
        let handle = unsafe { OpenEventW(access, FALSE, name.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a new handle that nothing else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    /// A new event of that name with the system's default descriptor: not the daemon's.
    #[allow(unsafe_code)]
    fn squat_event(name: &str, manual: bool) -> OwnedHandle {
        let name = wide(name);
        // SAFETY: null attributes are allowed, and `name` outlives the call.
        let handle =
            unsafe { CreateEventW(std::ptr::null(), i32::from(manual), FALSE, name.as_ptr()) };
        assert!(!handle.is_null(), "{}", io::Error::last_os_error());
        // SAFETY: a new handle that nothing else owns.
        unsafe { OwnedHandle::from_raw_handle(handle) }
    }

    /// Runs `f` on a thread whose token keeps every group but holds this account only to deny: to
    /// a private object's one ACE this is another account, on any machine and with no password.
    #[allow(unsafe_code)]
    fn as_another_account<T: Send>(f: impl FnOnce() -> T + Send) -> T {
        let me = sys::own_account().unwrap();
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo handle is always valid, and `token` is writable.
        let ok = unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY,
                &mut token,
            )
        };
        assert_ne!(ok, 0, "{}", io::Error::last_os_error());
        // SAFETY: a new handle that nothing else owns.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut sid = me.as_bytes().to_vec();
        let deny = SID_AND_ATTRIBUTES {
            Sid: sid.as_mut_ptr().cast(),
            Attributes: 0,
        };
        let mut restricted: HANDLE = std::ptr::null_mut();
        // SAFETY: `token` is open, `deny` holds one valid SID that outlives the call, the other
        // lists are empty, and `restricted` is writable.
        let ok = unsafe {
            CreateRestrictedToken(
                token.as_raw_handle(),
                0,
                1,
                &deny,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                &mut restricted,
            )
        };
        assert_ne!(ok, 0, "{}", io::Error::last_os_error());
        // SAFETY: a new handle that nothing else owns.
        let restricted = unsafe { OwnedHandle::from_raw_handle(restricted) };
        let mut acting: HANDLE = std::ptr::null_mut();
        // SAFETY: `restricted` is open, and `acting` is writable.
        let ok = unsafe {
            DuplicateToken(
                restricted.as_raw_handle(),
                SecurityImpersonation,
                &mut acting,
            )
        };
        assert_ne!(ok, 0, "{}", io::Error::last_os_error());
        // SAFETY: a new handle that nothing else owns.
        let acting = unsafe { OwnedHandle::from_raw_handle(acting) };
        std::thread::scope(|s| {
            s.spawn(|| {
                // SAFETY: a null thread is the calling one, and `acting` is an open impersonation
                // token. The thread ends after `f`, and its token with it.
                let ok = unsafe { SetThreadToken(std::ptr::null(), acting.as_raw_handle()) };
                assert_ne!(ok, 0, "{}", io::Error::last_os_error());
                f()
            })
            .join()
            .unwrap()
        })
    }

    #[test]
    fn wx16_event_acl_blocks_another_account() {
        let dir = project();
        let root = dir.path().canonicalize().unwrap();
        let name = event_of(&root);
        let rt = runtime();
        let _g = rt.enter();
        let mut signals = sys::StopSignals::install(&root).unwrap();

        // the object: this account owns it, and its one ACE is this account's. The system keeps
        // only the rights an event has
        let me = sys::own_account().unwrap();
        let shown = File::from(open_event(&name, READ_CONTROL).unwrap());
        let security = pio::security_of(&shown).unwrap();
        assert_eq!(security.owner, Some(me));
        assert!(security.protected);
        assert_eq!(
            security.dacl,
            Some(vec![Ace {
                mask: EVENT_ALL_ACCESS,
                ..Ace::private(me, ObjectKind::File)
            }])
        );

        // another account cannot open it, for any right, so it can neither signal nor hold it
        let denied = as_another_account(|| {
            [
                EVENT_MODIFY_STATE,
                SYNCHRONIZATION_SYNCHRONIZE,
                READ_CONTROL,
            ]
            .map(|access| open_event(&name, access).map(drop).unwrap_err())
        });
        for e in denied {
            assert_eq!(e.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32), "{e}");
        }
        // nor can the client's own call reach it
        let refused = as_another_account(|| sys::signal_stop_event(&name).unwrap_err());
        assert_eq!(refused.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));

        // this account's signal is what the daemon hears, after all of that
        sys::signal_stop_event(&name).unwrap();
        assert_eq!(rt.block_on(signals.recv()), "the stop event");
    }

    #[test]
    fn wx16_native_an_event_of_that_name_is_a_collision() {
        let dir = project();
        let root = dir.path().canonicalize().unwrap();
        let name = event_of(&root);
        let rt = runtime();
        let _g = rt.enter();
        let exists = io::Error::from_raw_os_error(ERROR_ALREADY_EXISTS as i32);
        let refusal = format!("could not create stop event {name}: {exists}");

        // another process's event of that name, whoever made it and however
        let squatter = squat_event(&name, true);
        let refused = sys::StopSignals::install(&root).map(drop).unwrap_err();
        assert_eq!(refused.to_string(), refusal);
        assert_eq!(refused.kind(), exists.kind());
        drop(squatter);

        // a daemon that holds it; the name comes free with the daemon
        let first = sys::StopSignals::install(&root).unwrap();
        let refused = sys::StopSignals::install(&root).map(drop).unwrap_err();
        assert_eq!(refused.to_string(), refusal);
        drop(first);
        drop(sys::StopSignals::install(&root).unwrap());
    }

    #[test]
    fn wx16_native_two_signals_are_two_stops() {
        // the second is what ends a drain at once, as a second SIGTERM does
        let dir = project();
        let root = dir.path().canonicalize().unwrap();
        let name = event_of(&root);
        let rt = runtime();
        let _g = rt.enter();
        let mut signals = sys::StopSignals::install(&root).unwrap();
        sys::signal_stop_event(&name).unwrap();
        assert_eq!(rt.block_on(signals.recv()), "the stop event");
        sys::signal_stop_event(&name).unwrap();
        assert_eq!(rt.block_on(signals.recv()), "the stop event");
    }

    #[test]
    fn wx16_native_no_event_is_a_signal_failure() {
        let name = event_of(Path::new(r"C:\no-daemon-of-rdy-has-this-root"));
        let refused = sys::signal_stop_event(&name).unwrap_err();
        assert_eq!(refused.raw_os_error(), Some(ERROR_FILE_NOT_FOUND as i32));

        // through the whole chain, on a real daemon that has none: its event was never made
        let dir = project();
        let mut daemon = Daemon::start(dir.path(), "no-event");
        let run = RunDir::of_project(dir.path());
        let pid = daemon.child.id();
        let got = runtime().block_on(async {
            let (endpoint, process, _stream) = refused_connection(&run).await;
            sys::stop_refused_daemon(run.clone(), endpoint, process, HANG_LIMIT).await
        });
        assert_eq!(
            shape(&got),
            format!("signal failed {pid} Some({})", ERROR_FILE_NOT_FOUND)
        );
        assert!(daemon.runs(), "a failed signal forces nothing");

        // and in the words a person reads
        let e = runtime()
            .block_on(request_stop(&run, &hello(&run), HANG_LIMIT))
            .unwrap_err();
        let error = io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32);
        assert_eq!(e.code, E_DAEMON_VERSION);
        assert_eq!(
            e.message,
            format!("could not send the stop signal to the daemon's process {pid}: {error}")
        );
        assert_eq!(
            e.fix,
            format!("end process {pid}, then run rdy serve --stop again")
        );
        assert!(daemon.runs(), "a failed signal forces nothing");
    }

    #[test]
    fn wx16_native_a_daemon_that_does_not_heed_is_ended_under_full_proof() {
        let dir = project();
        let mut daemon = Daemon::start(dir.path(), "deaf");
        let run = RunDir::of_project(dir.path());
        let pid = daemon.child.id();
        let got = runtime().block_on(async {
            let (endpoint, process, _stream) = refused_connection(&run).await;
            sys::stop_refused_daemon(run.clone(), endpoint, process, Duration::ZERO).await
        });
        assert_eq!(shape(&got), format!("terminated {pid}"));
        let status = daemon.child.wait().unwrap();
        assert_eq!(status.code(), Some(sys::FORCED_EXIT_CODE as i32));
    }

    /// The real grace period runs here, so this takes that long. Nothing asserts how long.
    #[test]
    fn wx16_native_request_stop_ends_a_daemon_that_does_not_heed() {
        let dir = project();
        let mut daemon = Daemon::start(dir.path(), "deaf");
        let run = RunDir::of_project(dir.path());
        let pid = daemon.child.id();
        let request = runtime()
            .block_on(request_stop(&run, &hello(&run), HANG_LIMIT))
            .unwrap()
            .expect("a daemon answers");
        assert_eq!(request.pid, Some(pid));
        assert!(request.signalled);
        assert_eq!(
            daemon.child.wait().unwrap().code(),
            Some(sys::FORCED_EXIT_CODE as i32)
        );
    }

    #[test]
    fn wx16_native_a_forged_pid_file_never_ends_a_process() {
        let dir = project();
        let other_dir = project();
        let mut daemon = Daemon::start(dir.path(), "deaf");
        let mut bystander = Daemon::start(other_dir.path(), "deaf");
        let run = RunDir::of_project(dir.path());
        let other_run = RunDir::of_project(other_dir.path());
        let rt = runtime();
        let daemon_pid = daemon.child.id();
        let bystander_pid = bystander.child.id();
        let (daemon_born, bystander_born) = rt.block_on(async {
            let (_, a, _s) = refused_connection(&run).await;
            let (_, b, _t) = refused_connection(&other_run).await;
            (a.created(), b.created())
        });
        let good_record = std::fs::read(run.socket_record()).unwrap();

        // each forgery, and what it forges
        let forged_pid = |pid: u32, born: u64| {
            write_private_durable(&run.pid_file(), &pipe_pid_record(pid, born)).unwrap();
        };
        let stop = |rt: &tokio::runtime::Runtime| {
            shape(&rt.block_on(async {
                let endpoint = run.socket();
                let stream = sys::connect(&endpoint).await.unwrap();
                let process =
                    sys::check_daemon(&stream, &endpoint, sys::own_account().unwrap()).unwrap();
                sys::stop_refused_daemon(run.clone(), endpoint, process, Duration::ZERO).await
            }))
        };

        // the bystander, by its whole identity
        forged_pid(bystander_pid, bystander_born);
        assert_eq!(stop(&rt), "untied");
        // the daemon's pid with another creation time
        forged_pid(daemon_pid, daemon_born + 1);
        assert_eq!(stop(&rt), "untied");
        // no pid file at all, while the record stands
        std::fs::remove_file(run.pid_file()).unwrap();
        assert_eq!(stop(&rt), "untied");
        // a pid file past its bound, and one that names an impossible pid
        write_private_durable(&run.pid_file(), &[b'1'; 65]).unwrap();
        assert_eq!(stop(&rt), "untied");
        write_private_durable(&run.pid_file(), b"0\n1\n").unwrap();
        assert_eq!(stop(&rt), "untied");
        forged_pid(daemon_pid, daemon_born);
        // a record that names the other project's pipe, then no record
        let other = std::fs::read(other_run.socket_record()).unwrap();
        write_private_durable(&run.socket_record(), &other).unwrap();
        assert_eq!(stop(&rt), "untied");
        std::fs::remove_file(run.socket_record()).unwrap();
        assert_eq!(stop(&rt), "untied");

        assert!(daemon.runs(), "the daemon was ended");
        assert!(bystander.runs(), "the bystander was ended");

        // all four proofs again, so nothing above was refused for some other reason
        write_private_durable(&run.socket_record(), &good_record).unwrap();
        assert_eq!(stop(&rt), format!("terminated {daemon_pid}"));
        assert_eq!(
            daemon.child.wait().unwrap().code(),
            Some(sys::FORCED_EXIT_CODE as i32)
        );
        assert!(bystander.runs(), "the bystander was ended");
    }

    #[test]
    fn wx16_native_a_daemon_that_has_exited_is_left_alone() {
        let dir = project();
        let mut daemon = Daemon::start(dir.path(), "heeds");
        let run = RunDir::of_project(dir.path());
        let pid = daemon.child.id();
        let rt = runtime();
        let got = rt.block_on(async {
            let (endpoint, process, _stream) = refused_connection(&run).await;
            // the daemon goes between its refusal and the stop; the handle keeps its pid
            sys::signal_stop_event(&event_of(&run.root())).unwrap();
            daemon.heard("stopped by");
            daemon.child.wait().unwrap();
            sys::stop_refused_daemon(run.clone(), endpoint, process, HANG_LIMIT).await
        });
        assert_eq!(shape(&got), format!("exited {pid}"));
    }

    #[test]
    fn wx16_native_access_denied_is_a_termination_failure() {
        let dir = project();
        let mut daemon = Daemon::start(dir.path(), "guarded");
        daemon.heard("guarded");
        let run = RunDir::of_project(dir.path());
        let pid = daemon.child.id();
        let got = runtime().block_on(async {
            let (endpoint, process, _stream) = refused_connection(&run).await;
            sys::stop_refused_daemon(run.clone(), endpoint, process, Duration::ZERO).await
        });
        // an elevated token has a right past the process's own list, and then the proofs decide
        if can_end(pid) {
            assert_eq!(shape(&got), format!("terminated {pid}"));
        } else {
            assert_eq!(
                shape(&got),
                format!("terminate failed {pid} Some({ERROR_ACCESS_DENIED})")
            );
            assert!(daemon.runs());
        }
    }

    /// Whether this process may end `pid`, by the system's own check.
    #[allow(unsafe_code)]
    fn can_end(pid: u32) -> bool {
        // SAFETY: the call takes no pointer.
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, FALSE, pid) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: a new handle that nothing else owns; dropping it closes it.
        drop(unsafe { OwnedHandle::from_raw_handle(handle) });
        true
    }

    #[test]
    fn wx16_native_liveness_is_the_systems_word() {
        // no process can be asked about pid 0, so nothing is proved of it
        assert_eq!(sys::process_liveness(0), Liveness::Ambiguous);
        // the system's own process: open to no ordinary account, and it runs
        assert_eq!(sys::process_liveness(4), Liveness::Live);
        // a process that has ended, whose handle is still held: its pid is still taken
        let mut child = this_binary("none")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_remove(ROLE)
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(sys::process_liveness(pid), Liveness::Gone);
        assert!(!rdy_daemon::process_alive(pid));
        drop(child);
    }

    #[test]
    fn wx16_detach_does_not_inherit_store_or_console_handles() {
        let dir = project();
        let lock = dir.path().join("store-lock");
        let log = dir.path().join("detached.log");
        std::fs::File::create(&lock).unwrap();
        let agent = PathBuf::from(format!(r"\\.\pipe\rdy-wx16-agent-{}", std::process::id()));
        let release_name = format!(r"Local\rdy-wx16-release-{}", std::process::id());
        let release = squat_event(&release_name, true);
        let rt = runtime();
        let mut listener = rt.block_on(async { sys::bind(&agent) }).unwrap();

        // what a host does: it runs rdy with its own console and its own pipes as stdio
        let mut launcher = this_binary("launcher")
            .env(LOCK, &lock)
            .env(LOG, &log)
            .env(AGENT, &agent)
            .env(RELEASE, &release_name)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = launcher.stdin.take().unwrap();
        let mut out = launcher.stdout.take().unwrap();
        let mut err = launcher.stderr.take().unwrap();
        let mut agent_side = rt.block_on(listener.accept()).unwrap();

        // the host's stdout and stderr end when the launcher does, with the daemon still there
        let (tx, ended) = channel();
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            let mut text = String::new();
            out.read_to_string(&mut text).unwrap();
            tx.send(("stdout", text)).unwrap();
        });
        std::thread::spawn(move || {
            let mut text = String::new();
            err.read_to_string(&mut text).unwrap();
            tx2.send(("stderr", text)).unwrap();
        });
        let (mut said, mut told) = (String::new(), String::new());
        for _ in 0..2 {
            let (which, text) = ended
                .recv_timeout(HANG_LIMIT)
                .expect("the detached child holds the host's stdout or stderr");
            match which {
                "stdout" => said = text,
                _ => told = text,
            }
        }
        assert!(launcher.wait().unwrap().success(), "{said}{told}");
        let pid: u32 = said
            .lines()
            .find_map(|l| l.strip_prefix("detached="))
            .unwrap_or_else(|| panic!("{said}{told}"))
            .trim()
            .parse()
            .unwrap();
        assert!(rdy_daemon::process_alive(pid), "the detached child left");

        // the host's stdin has no reader left
        stdin
            .write_all(b"x")
            .and_then(|()| stdin.flush())
            .expect_err("the detached child holds the host's stdin");

        // the store's lock is free
        let again = std::fs::OpenOptions::new().write(true).open(&lock).unwrap();
        again
            .try_lock()
            .expect("the detached child holds the store's handle");

        // the agent's connection has ended
        let mut heard = Vec::new();
        let read = rt.block_on(async {
            use tokio::io::AsyncReadExt;
            tokio::time::timeout(HANG_LIMIT, agent_side.read_to_end(&mut heard)).await
        });
        read.expect("the detached child holds the agent's connection")
            .unwrap();

        // and it has no console, and leaving one again is no failure
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("console=none"), "{text}");
        assert!(text.contains("detach=ok"), "{text}");
        assert!(text.contains("signals=the stop event"), "{text}");
        assert!(rdy_daemon::process_alive(pid), "the detached child left");

        #[allow(unsafe_code)]
        // SAFETY: the handle is open for the call.
        unsafe {
            SetEvent(release.as_raw_handle())
        };
        while sys::process_liveness(pid) != Liveness::Gone {
            std::thread::yield_now();
        }
    }

    /// The launcher: it holds what `rdy` would hold when it starts a daemon, starts one as
    /// `serve_command` does, and leaves it behind.
    #[allow(unsafe_code)]
    fn launcher() {
        // SAFETY: the call takes nothing.
        assert_ne!(unsafe { GetConsoleCP() }, 0, "the launcher has no console");
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(std::env::var_os(LOCK).unwrap())
            .unwrap();
        lock.try_lock().unwrap();
        let agent = PathBuf::from(std::env::var_os(AGENT).unwrap());
        let cancel = sys::AgentCancel::new().unwrap();
        let held_agent = sys::agent_connect(&agent, &cancel).unwrap();
        let log = std::env::var_os(LOG).unwrap();
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&log)
            .unwrap();
        let mut detached = this_binary("detached");
        detached
            .stdin(Stdio::null())
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file));
        sys::new_session_on_spawn(&mut detached);
        rdy_daemon::spawn_serve(&mut detached).unwrap();
        // the child's last word: it runs, with everything it will ever inherit
        let pid = loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.contains("signals=") {
                let pid = text.lines().find_map(|l| l.strip_prefix("pid="));
                break pid.unwrap().trim().to_string();
            }
            std::thread::yield_now();
        };
        println!("\ndetached={pid}");
        drop(held_agent);
        // this process's exit is what lets go of the rest; a Child never kills on drop
    }

    /// The detached child: it reports, then waits for the test's word.
    #[allow(unsafe_code)]
    fn detached() {
        // SAFETY: the call takes nothing.
        let console = if unsafe { GetConsoleCP() } == 0 {
            "console=none"
        } else {
            "console=some"
        };
        println!("\n{console}");
        println!("pid={}", std::process::id());
        match rdy_daemon::detach() {
            Ok(()) => println!("detach=ok"),
            Err(e) => println!("detach=failed {e}"),
        }
        // the daemon's own stop works with no console
        let lock = PathBuf::from(std::env::var_os(LOCK).unwrap());
        let root = lock.parent().unwrap().canonicalize().unwrap();
        let rt = runtime();
        let _g = rt.enter();
        let mut signals = sys::StopSignals::install(&root).unwrap();
        sys::signal_stop_event(&event_of(&root)).unwrap();
        println!("signals={}", rt.block_on(signals.recv()));
        let release = open_event(
            &std::env::var(RELEASE).unwrap(),
            SYNCHRONIZATION_SYNCHRONIZE,
        )
        .unwrap();
        // SAFETY: the handle is open for the call.
        let waited = unsafe { WaitForSingleObject(release.as_raw_handle(), INFINITE) };
        assert_eq!(waited, WAIT_OBJECT_0);
    }

    /// Serves with no event at all, so a client's signal finds none.
    fn no_event() {
        runtime().block_on(async {
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            let (listener, _, _) = socket::bind(&RunDir::of_project(&root)).await.unwrap();
            println!("\nready");
            refuse_every_hello(listener).await;
        });
    }

    /// This process's handle to itself allows only what a look needs. A client may then neither
    /// end it nor do anything else to it, though it is the same account.
    #[allow(unsafe_code)]
    fn guard_this_process() {
        let me = sys::own_account().unwrap();
        let mut sid = me.as_bytes().to_vec();
        let mut acl = vec![0u32; 64];
        let size = (acl.len() * 4) as u32;
        let acl_ptr: *mut ACL = acl.as_mut_ptr().cast();
        // SAFETY: an all-zero descriptor is a plain value, and it is initialized below.
        let mut descriptor: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
        let sd: *mut SECURITY_DESCRIPTOR = &mut descriptor;
        // SAFETY: the buffer is 4-byte aligned, writable and of the size given, and big enough for
        // one ACE (`ACCESS_ALLOWED_ACE` and one SID); `sid` and `descriptor` outlive these calls.
        unsafe {
            assert_ne!(InitializeAcl(acl_ptr, size, ACL_REVISION), 0);
            assert_ne!(
                AddAccessAllowedAce(
                    acl_ptr,
                    ACL_REVISION,
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    sid.as_mut_ptr().cast(),
                ),
                0
            );
            assert_ne!(InitializeSecurityDescriptor(sd.cast(), 1), 0);
            assert_ne!(SetSecurityDescriptorDacl(sd.cast(), 1, acl_ptr, 0), 0);
            assert_ne!(
                SetKernelObjectSecurity(GetCurrentProcess(), DACL_SECURITY_INFORMATION, sd.cast()),
                0,
                "{}",
                io::Error::last_os_error()
            );
        }
    }

    pub(super) fn child_role(role: &str) {
        match role {
            "deaf" => runtime().block_on(serve_until(async |_signals: sys::StopSignals| {
                std::future::pending::<()>().await;
            })),
            "guarded" => runtime().block_on(serve_until(async |_signals: sys::StopSignals| {
                guard_this_process();
                println!("\nguarded");
                std::future::pending::<()>().await;
            })),
            "no-event" => no_event(),
            "launcher" => launcher(),
            "detached" => detached(),
            other => panic!("no role {other}"),
        }
    }
}
