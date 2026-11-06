//! The caller's SSH agent on Windows: the named pipe its `SSH_AUTH_SOCK` names, and the caller's
//! interrupt while it signs.
//!
//! - **Only the name the caller gives.** Nothing here looks for the system's agent service. The
//!   name must be a local pipe, `\\.\pipe\<name>` with either slash. A path, a device or a remote
//!   pipe is refused before anything opens it. An agent sees that the client's token is this
//!   account's, and cannot act as that account: the handle allows identification only.
//! - **Raw bytes.** An agent is no rdy process, so this stream has no half-close layer.
//! - **Cancellable.** Every read and write is overlapped on the caller's own thread, and waits
//!   for its result or for [`agent_close`]. A closed stream cancels the operation, and the thread
//!   waits until the system has let go of the buffer. There is no helper thread.
//! - **Not inherited.** No handle here is inheritable, so a child never holds the connection.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED, FALSE,
    GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, TRUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

/// How long a connect waits before it tries a busy pipe again. It is a retry pace, not a timeout:
/// the connect goes on until it connects, fails or is cancelled.
const AGENT_CONNECT_RETRY_MS: u32 = 20;

/// A manual-reset event: once set, it stays set.
struct Event(OwnedHandle);

impl Event {
    #[allow(unsafe_code)]
    fn new() -> io::Result<Event> {
        // SAFETY: null attributes and a null name are allowed. The call reads nothing else.
        let event = unsafe { CreateEventW(null(), TRUE, FALSE, null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `event` is a new handle that nothing else owns.
        Ok(Event(unsafe { OwnedHandle::from_raw_handle(event) }))
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle()
    }

    #[allow(unsafe_code)]
    fn set(&self) {
        // SAFETY: the handle is open for as long as `self`.
        unsafe { SetEvent(self.raw()) };
    }

    /// Waits for at most `ms` milliseconds: whether the event is set.
    #[allow(unsafe_code)]
    fn wait_for(&self, ms: u32) -> bool {
        // SAFETY: the handle is open for as long as `self`.
        unsafe { WaitForSingleObject(self.raw(), ms) == WAIT_OBJECT_0 }
    }
}

/// Ends a pending [`agent_connect`] from another thread. Once set, it stays set.
pub struct AgentCancel(Event);

impl AgentCancel {
    /// A new cancel, not set.
    pub fn new() -> io::Result<AgentCancel> {
        Ok(AgentCancel(Event::new()?))
    }

    /// Sets the cancel: a connect that waits with it ends as `Interrupted`. Setting it again does
    /// nothing.
    pub fn cancel(&self) {
        self.0.set();
    }

    /// Whether the cancel is set.
    pub fn is_set(&self) -> bool {
        self.0.wait_for(0)
    }

    /// Waits until the cancel is set.
    pub fn wait(&self) {
        self.0.wait_for(INFINITE);
    }
}

/// The parts of a connection that [`agent_close`] needs. The pipe closes with the last holder.
struct Shared {
    pipe: OwnedHandle,
    /// Set by [`agent_close`]: the operation in progress ends, and no other starts.
    closed: Event,
}

/// A blocking connection to the caller's SSH agent. It is used only on a request's blocking
/// worker, never on the daemon's writer, and lives for one signing call.
pub struct AgentStream {
    shared: Arc<Shared>,
    /// The completion event of this stream's one operation at a time.
    done: Event,
}

/// A second hold on an [`AgentStream`], so another thread can close it while a read waits.
pub struct AgentCloser(Arc<Shared>);

/// The error of a closed stream. It is not `Interrupted`, which `read_exact` and `write_all` retry.
fn stream_closed() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "agent stream closed")
}

impl AgentStream {
    fn pipe(&self) -> HANDLE {
        self.shared.pipe.as_raw_handle()
    }

    /// Runs one overlapped operation that `start` begins, to its end: the bytes moved. A close
    /// cancels it. This returns only when the system has finished with `start`'s buffer and with
    /// the operation's record, whatever happened.
    #[allow(unsafe_code)]
    fn transfer(
        &mut self,
        start: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32,
    ) -> io::Result<usize> {
        if self.shared.closed.wait_for(0) {
            return Err(stream_closed());
        }
        // SAFETY: all zeros is a valid idle OVERLAPPED.
        let mut record: OVERLAPPED = unsafe { std::mem::zeroed() };
        record.hEvent = self.done.raw();
        if start(self.pipe(), &raw mut record) == 0 {
            // SAFETY: this reads this thread's last error and touches no memory of ours.
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            let waits = [self.done.raw(), self.shared.closed.raw()];
            // SAFETY: `waits` holds two handles that `self` keeps open during the wait.
            let woken = unsafe { WaitForMultipleObjects(2, waits.as_ptr(), FALSE, INFINITE) };
            if woken != WAIT_OBJECT_0 {
                // SAFETY: `record` is the operation in progress, and stays alive below. An
                // operation that has just ended makes this fail, which changes nothing.
                unsafe { CancelIoEx(self.pipe(), &raw const record) };
            }
        }
        let mut moved = 0u32;
        // SAFETY: `record` and `moved` are live, and the wait makes the system finish with them
        // before the call returns.
        let finished =
            unsafe { GetOverlappedResult(self.pipe(), &raw const record, &raw mut moved, TRUE) };
        if finished == 0 {
            let error = io::Error::last_os_error();
            if self.shared.closed.wait_for(0) {
                return Err(stream_closed());
            }
            return Err(error);
        }
        Ok(moved as usize)
    }
}

impl io::Read for AgentStream {
    #[allow(unsafe_code)]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let at = buf.as_mut_ptr();
        // SAFETY: `at` is writable for `len` bytes, and `transfer` keeps `buf` borrowed until the
        // system has finished with it. An overlapped call takes no count pointer.
        let read =
            self.transfer(|pipe, record| unsafe { ReadFile(pipe, at, len, null_mut(), record) });
        match read {
            // The agent has closed its end: the end of the stream.
            Err(e) if is_pipe_end(&e) => Ok(0),
            other => other,
        }
    }
}

fn is_pipe_end(e: &io::Error) -> bool {
    let code = e.raw_os_error().and_then(|n| u32::try_from(n).ok());
    code == Some(ERROR_BROKEN_PIPE) || code == Some(ERROR_PIPE_NOT_CONNECTED)
}

impl io::Write for AgentStream {
    #[allow(unsafe_code)]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let at = buf.as_ptr();
        // SAFETY: `at` is readable for `len` bytes, and `transfer` keeps `buf` borrowed until the
        // system has finished with it. An overlapped call takes no count pointer.
        self.transfer(|pipe, record| unsafe { WriteFile(pipe, at, len, null_mut(), record) })
    }

    /// A write ends when the pipe has the bytes, so nothing waits here.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The error of a cancelled connect.
fn agent_cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "agent connect cancelled")
}

const PIPE_PREFIX: &str = r"\\.\pipe\";

/// `path` as the wide, NUL-ended name of a local pipe, with either slash read as a backslash.
/// Anything else is refused: a dot-dot could lead out of the pipe namespace.
fn pipe_name(path: &Path) -> io::Result<Vec<u16>> {
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    let refused = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the agent endpoint is not a local named pipe",
        )
    };
    let name: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .map(|c| {
            if c == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                c
            }
        })
        .collect();
    let prefix = wide(PIPE_PREFIX);
    if name.len() <= prefix.len() || name.contains(&0) {
        return Err(refused());
    }
    let (head, rest) = name.split_at(prefix.len());
    let lower = |c: &u16| u8::try_from(*c).map_or(*c, |b| u16::from(b.to_ascii_lowercase()));
    if !head.iter().map(lower).eq(prefix.iter().copied()) {
        return Err(refused());
    }
    let (dot, dots) = (wide("."), wide(".."));
    if rest
        .split(|c| *c == u16::from(b'\\'))
        .any(|part| part == dot || part == dots)
    {
        return Err(refused());
    }
    let mut out = name;
    out.push(0);
    Ok(out)
}

/// Connects to the agent's pipe `path`, on the caller's thread. A busy pipe is tried again until
/// it connects or fails, or until `cancel` is set from another thread: then it ends as
/// `Interrupted`. No handle is held between tries, and there is no helper thread.
#[allow(unsafe_code)]
pub fn agent_connect(path: &Path, cancel: &AgentCancel) -> io::Result<AgentStream> {
    let name = pipe_name(path)?;
    let closed = Event::new()?;
    let done = Event::new()?;
    loop {
        if cancel.is_set() {
            return Err(agent_cancelled());
        }
        // SAFETY: `name` is NUL-ended and live for the call. Null attributes make the handle not
        // inheritable, and no template is given.
        let pipe = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if pipe != INVALID_HANDLE_VALUE {
            // SAFETY: `pipe` is a new handle that nothing else owns.
            let pipe = unsafe { OwnedHandle::from_raw_handle(pipe) };
            return Ok(AgentStream {
                shared: Arc::new(Shared { pipe, closed }),
                done,
            });
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error().and_then(|n| u32::try_from(n).ok()) != Some(ERROR_PIPE_BUSY) {
            return Err(error);
        }
        if cancel.0.wait_for(AGENT_CONNECT_RETRY_MS) {
            return Err(agent_cancelled());
        }
    }
}

/// A second hold on `stream`, so another thread can close it while a read waits.
pub fn agent_handle(stream: &AgentStream) -> io::Result<AgentCloser> {
    Ok(AgentCloser(Arc::clone(&stream.shared)))
}

/// Closes the stream of `closer`: a read or write that waits on it ends, and no other starts. The
/// agent sees the end when the call lets go of the stream.
pub fn agent_close(closer: &AgentCloser) {
    closer.0.closed.set();
}

// The caller's interrupt while it signs

/// The guards alive. The handler is installed while there is one.
static HOLDERS: Mutex<usize> = Mutex::new(0);
/// Counts the interrupts, for every guard.
static COUNT: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();
/// How many interrupts the handler has taken. Test code reads it.
static TAKEN: AtomicUsize = AtomicUsize::new(0);

fn holders() -> MutexGuard<'static, usize> {
    HOLDERS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn count() -> &'static tokio::sync::watch::Sender<u64> {
    COUNT.get_or_init(|| tokio::sync::watch::channel(0u64).0)
}

/// Ctrl-C and Ctrl-Break are this process's to handle while a guard lives. Any other event goes
/// to the next handler. The system runs this on a thread of its own.
unsafe extern "system" fn on_console_event(event: u32) -> i32 {
    if event != CTRL_C_EVENT && event != CTRL_BREAK_EVENT {
        return FALSE;
    }
    TAKEN.fetch_add(1, Ordering::SeqCst);
    count().send_modify(|c| *c = c.wrapping_add(1));
    TRUE
}

/// Ctrl-C and Ctrl-Break caught while the guard lives: [`InterruptGuard::recv`] ends at the next
/// one. With no guard alive, they do what they did before the first guard.
pub struct InterruptGuard {
    seen: tokio::sync::watch::Receiver<u64>,
}

impl InterruptGuard {
    /// Waits for an interrupt caught since the guard was made.
    pub async fn recv(&mut self) {
        if self.seen.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

impl Drop for InterruptGuard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        let mut alive = holders();
        *alive = alive.saturating_sub(1);
        if *alive == 0 {
            // SAFETY: the handler is a function of this program, and this only takes it off the
            // system's list.
            unsafe { SetConsoleCtrlHandler(Some(on_console_event), FALSE) };
        }
    }
}

/// Catches Ctrl-C and Ctrl-Break until the guard is dropped (guards may overlap). Call it before
/// the work an interrupt should stop begins, so an interrupt during that work is never missed.
#[allow(unsafe_code)]
pub fn catch_interrupt() -> io::Result<InterruptGuard> {
    let mut alive = holders();
    let seen = count().subscribe();
    if *alive == 0 {
        // SAFETY: the handler is a function of this program, safe to run on any thread.
        if unsafe { SetConsoleCtrlHandler(Some(on_console_event), TRUE) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    *alive += 1;
    Ok(InterruptGuard { seen })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::mpsc::channel;
    use std::thread;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
    use windows_sys::Win32::Security::{
        GetTokenInformation, RevertToSelf, SECURITY_IMPERSONATION_LEVEL, SecurityIdentification,
        TOKEN_QUERY, TokenImpersonationLevel,
    };
    use windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent;
    use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

    /// A hung test fails here. No test asserts timing.
    const HANG_LIMIT: Duration = Duration::from_secs(120);
    /// A new console for a child, so its console events reach nobody else.
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    /// The exit status of a process that Ctrl-C ended.
    const STATUS_CONTROL_C_EXIT: u32 = 0xC000_013A;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn name() -> PathBuf {
        PathBuf::from(format!(
            r"\\.\pipe\rdy-wx18-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ))
    }

    fn accepted(path: &str) -> String {
        let wide = pipe_name(Path::new(path)).unwrap();
        String::from_utf16(&wide[..wide.len() - 1]).unwrap()
    }

    #[test]
    fn wx18_native_only_a_local_pipe_name_is_taken() {
        assert_eq!(
            accepted(r"\\.\pipe\openssh-ssh-agent"),
            r"\\.\pipe\openssh-ssh-agent"
        );
        assert_eq!(accepted("//./pipe/agent"), r"\\.\pipe\agent");
        assert_eq!(accepted(r"\\.\PIPE\Agent"), r"\\.\PIPE\Agent");
        assert_eq!(accepted(r"\\.\pipe\a\b.c"), r"\\.\pipe\a\b.c");
        for refused in [
            "",
            r"\\.\pipe\",
            "//./pipe/",
            r"C:\Users\me\agent.sock",
            "/tmp/ssh-x/agent.1",
            "agent.sock",
            r"\\server\pipe\agent",
            r"\\?\pipe\agent",
            r"\\.\C:\agent",
            r"\\.\COM1",
            r"\\.\pipex\agent",
            r"\\.\pipe\..\C:\agent",
            r"\\.\pipe\a\..\..\C:\agent",
            "//./pipe/../C:/agent",
            r"\\.\pipe\.",
            "\\\\.\\pipe\\a\0b",
        ] {
            let e = pipe_name(Path::new(refused)).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{refused:?}");
            // and it never reaches the system: a connect gives the same refusal
            let e = agent_connect(Path::new(refused), &AgentCancel::new().unwrap())
                .err()
                .unwrap();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{refused:?}");
        }
    }

    #[tokio::test]
    async fn wx18_native_a_busy_pipe_waits_for_its_instance_or_its_cancel() {
        let path = name();
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .create(&path)
            .unwrap();
        let first = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || {
                agent_connect(&path, &AgentCancel::new().unwrap()).map(|_| ())
            })
        };
        server.connect().await.unwrap();
        first.await.unwrap().unwrap();

        // The one instance has its client (still connected until it disconnects): a connect waits,
        // and only its cancel ends it.
        let cancel = Arc::new(AgentCancel::new().unwrap());
        let waiting = {
            let (path, cancel) = (path.clone(), Arc::clone(&cancel));
            tokio::task::spawn_blocking(move || {
                agent_connect(&path, &cancel)
                    .map(|_| ())
                    .map_err(|e| e.kind())
            })
        };
        cancel.cancel();
        assert_eq!(waiting.await.unwrap(), Err(io::ErrorKind::Interrupted));

        // A wait ends in a connection when the instance is free again.
        let waiting = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || {
                agent_connect(&path, &AgentCancel::new().unwrap()).map(|_| ())
            })
        };
        server.disconnect().unwrap();
        server.connect().await.unwrap();
        waiting.await.unwrap().unwrap();
    }

    #[test]
    fn wx18_native_no_such_pipe_fails_at_once() {
        let e = agent_connect(&name(), &AgentCancel::new().unwrap())
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    /// Makes the pipe and connects a client: the server's end, and the client.
    async fn pair() -> (NamedPipeServer, AgentStream) {
        let path = name();
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&path)
            .unwrap();
        let client = agent_connect(&path, &AgentCancel::new().unwrap()).unwrap();
        server.connect().await.unwrap();
        (server, client)
    }

    #[tokio::test]
    async fn wx18_native_the_stream_is_raw_bytes_both_ways() {
        // Every byte value, 0xFF too: an agent speaks no escape.
        let data: Vec<u8> = (0..=u8::MAX).collect();
        let (mut server, mut client) = pair().await;
        client.write_all(&data).unwrap();
        client.flush().unwrap();
        let mut got = vec![0u8; data.len()];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(got, data);
        server.write_all(&data).await.unwrap();
        let mut got = vec![0u8; data.len()];
        tokio::task::spawn_blocking(move || {
            client.read_exact(&mut got).unwrap();
            got
        })
        .await
        .map(|got| assert_eq!(got, data))
        .unwrap();
    }

    #[tokio::test]
    async fn wx18_native_the_agent_closing_is_the_end_of_the_stream() {
        let (mut server, mut client) = pair().await;
        server.write_all(b"last").await.unwrap();
        let _ = server.write(&[]).await;
        drop(server);
        let read = tokio::task::spawn_blocking(move || {
            let mut got = Vec::new();
            client.read_to_end(&mut got).map(|_| got)
        });
        assert_eq!(read.await.unwrap().unwrap(), b"last");
    }

    #[tokio::test]
    async fn wx18_native_no_handle_is_inherited() {
        let (_server, client) = pair().await;
        for handle in [client.pipe(), client.done.raw(), client.shared.closed.raw()] {
            let mut flags = 0u32;
            // SAFETY: the handle is open, and `flags` is live and writable for the call.
            #[allow(unsafe_code)]
            let read = unsafe { GetHandleInformation(handle, &raw mut flags) };
            assert_ne!(read, 0);
            assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        }
    }

    /// What the client's connection lets the pipe's server do with its token.
    #[allow(unsafe_code)]
    fn level_the_server_gets(server: &NamedPipeServer) -> SECURITY_IMPERSONATION_LEVEL {
        // SAFETY: the handles and out values are live for each call. The impersonation is on this
        // thread and ends before this returns.
        unsafe {
            assert_ne!(ImpersonateNamedPipeClient(server.as_raw_handle()), 0);
            let mut token: HANDLE = null_mut();
            let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, TRUE, &raw mut token);
            let error = io::Error::last_os_error();
            assert_ne!(RevertToSelf(), 0);
            assert_ne!(opened, 0, "{error}");
            let token = OwnedHandle::from_raw_handle(token);
            let mut level: SECURITY_IMPERSONATION_LEVEL = 0;
            let mut len = 0u32;
            assert_ne!(
                GetTokenInformation(
                    token.as_raw_handle(),
                    TokenImpersonationLevel,
                    (&raw mut level).cast(),
                    size_of::<SECURITY_IMPERSONATION_LEVEL>() as u32,
                    &raw mut len,
                ),
                0
            );
            level
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wx18_native_a_pipe_server_can_only_identify_the_caller() {
        let (mut server, mut client) = pair().await;
        client.write_all(b"x").unwrap();
        let mut one = [0u8; 1];
        server.read_exact(&mut one).await.unwrap();
        // no await while this thread impersonates
        assert_eq!(level_the_server_gets(&server), SecurityIdentification);
    }

    #[tokio::test]
    async fn wx18_interrupt_handler_counts_only_ctrl_c_and_break() {
        let mut guard = catch_interrupt().unwrap();
        let mut other = catch_interrupt().unwrap();
        // SAFETY: the handler takes an integer and touches only its own statics.
        #[allow(unsafe_code)]
        let (close, c, brk) = unsafe {
            (
                on_console_event(windows_sys::Win32::System::Console::CTRL_CLOSE_EVENT),
                on_console_event(CTRL_C_EVENT),
                on_console_event(CTRL_BREAK_EVENT),
            )
        };
        assert_eq!((close, c, brk), (FALSE, TRUE, TRUE));
        guard.recv().await;
        other.recv().await;
        drop(other);
        assert!(TAKEN.load(Ordering::SeqCst) >= 2);
    }

    /// The child of the next test, on a console of its own: a real Ctrl-C while the guard lives
    /// wakes the guard, and one after it ends the process as before.
    #[test]
    #[ignore = "wx18_native_a_real_ctrl_c_is_caught_only_inside_the_guard runs this in a child"]
    #[allow(unsafe_code)]
    fn wx18_child_a_real_ctrl_c() {
        // SAFETY: a null handler with FALSE only clears an ignore flag from the parent.
        assert_ne!(unsafe { SetConsoleCtrlHandler(None, FALSE) }, 0);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for event in [CTRL_C_EVENT, CTRL_BREAK_EVENT] {
            let mut guard = catch_interrupt().unwrap();
            // SAFETY: group 0 is this console's own, and only this process (and no other program)
            // is on the console the parent made for it.
            assert_ne!(unsafe { GenerateConsoleCtrlEvent(event, 0) }, 0);
            rt.block_on(async {
                tokio::time::timeout(HANG_LIMIT, guard.recv())
                    .await
                    .expect("the interrupt never came");
            });
            drop(guard);
        }
        // SAFETY: as above. With no guard the default action ends this process.
        assert_ne!(unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) }, 0);
        thread::sleep(HANG_LIMIT);
        std::process::exit(3);
    }

    #[test]
    fn wx18_native_a_real_ctrl_c_is_caught_only_inside_the_guard() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sys::windows::agent::tests::wx18_child_a_real_ctrl_c",
                "--ignored",
                "--nocapture",
            ])
            .creation_flags(CREATE_NEW_CONSOLE)
            .status()
            .unwrap();
        assert_eq!(status.code().map(|c| c as u32), Some(STATUS_CONTROL_C_EXIT));
    }

    #[tokio::test]
    async fn wx18_native_close_ends_a_blocked_read_of_this_thread_only() {
        let (mut server, mut client) = pair().await;
        let closer = agent_handle(&client).unwrap();
        let (started, on_start) = channel();
        let read = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            let r = client.read(&mut [0u8; 16]).map_err(|e| e.kind());
            (r, client)
        });
        on_start.recv().unwrap();
        agent_close(&closer);
        let (r, mut client) = read.await.unwrap();
        assert_eq!(r, Err(io::ErrorKind::ConnectionAborted));
        // no read or write starts after the close
        assert_eq!(
            client.write(b"x").unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        // the agent sees the end only when the last hold goes
        drop(client);
        drop(closer);
        let mut got = Vec::new();
        assert_eq!(server.read_to_end(&mut got).await.unwrap_or(0), 0);
    }
}
