//! Private child fixtures for the harness's own tests (FW-XP1 D-1RXEA). A fixture child is the
//! libtest executable that is already running: the test binary starts itself again with libtest's
//! `--exact` selector for [`fixture_entry`] and a private environment variable that names the mode.
//! There is no product executable, tool or flag; the whole module is registered under `cfg(test)`, so a product
//! release holds none of this. The same bytes run on every platform, so the tests that need a hanging, noisy or failing child work the same way on Unix and Windows.
//!
//! Modes (the value of [`MODE_ENV`]):
//! - `exit:<code>`: exit with `code`.
//! - `output:<stdout|stderr>:<bytes>`: write that many `x` bytes to the stream and exit 0.
//! - `invalid:<stdout|stderr>`: write [`INVALID_BYTES`], which are not UTF-8, and exit 0.
//! - `hang`: never exit by itself.
//! - `descendants:<count>`: start `count` hanging copies of this child, write their process IDs to
//!   the file named by [`TARGET_ENV`], one per line, and hang. The copies share the streams.
//! - `holder`: the escaped pipe holder. Put the streams in a process outside the starter's
//!   process tree, write a report (see [`Held`]) to the file named by [`TARGET_ENV`] and exit 0 at
//!   once. On Unix the child starts a hanging copy in a process group of its own. On Windows a
//!   job without breakaway refuses any child outside the job, so the caller starts an
//!   independent hanging process first and names it in [`HOLDER_ENV`]; the child lends that
//!   process both of its streams. The report is written last, so it also says that the transfer
//!   is complete.
//! - `editor`: write [`TEXT_ENV`] to the file named by [`TARGET_ENV`], as an editor does, and
//!   exit 0.
//!
//! libtest prints its own header to stdout before a test starts, so a mode that needs exact bytes
//! writes them to stderr, or the caller accounts for the header.
//!
//! Reuse: this file uses only `std`. An integration binary includes it with
//! `#[path = "../src/platform/child_fixture.rs"] mod child_fixture;`, which installs
//! [`fixture_entry`] in that binary, and builds its children with [`fixture_command`], which
//! selects the entry by [`entry_selector`].

#![allow(dead_code)]

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// The environment variable that selects the mode. Unset, the entry test does nothing.
pub const MODE_ENV: &str = "RDY_QA_CHILD_FIXTURE";
/// The environment variable that names the file a mode writes to.
pub const TARGET_ENV: &str = "RDY_QA_CHILD_TARGET";
/// The environment variable that holds the text the `editor` mode writes.
pub const TEXT_ENV: &str = "RDY_QA_CHILD_TEXT";
/// The environment variable that names the independent process (a process ID) the `holder` mode
/// lends its streams to. Windows only; Unix starts its own holder.
pub const HOLDER_ENV: &str = "RDY_QA_CHILD_HOLDER";
/// The bytes of the `invalid` mode: not valid UTF-8, with a newline and a NUL between them.
pub const INVALID_BYTES: &[u8] = &[0xff, 0xfe, 0x80, b'\n', 0xc0, 0xaf, 0x00, 0xed, 0xa0, 0x80];

/// The libtest name that `--exact` selects for [`fixture_entry`] in the binary this file is in:
/// the module path without the crate name. A binary that includes this file under another module
/// name gets its own selector.
pub fn entry_selector() -> String {
    let path = module_path!();
    let inside = path.split_once("::").map_or("", |(_, rest)| rest);
    format!("{inside}::fixture_entry")
}

/// A stream a mode writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One parsed fixture mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Exit(i32),
    Output(Stream, usize),
    Invalid(Stream),
    Hang,
    Descendants(usize),
    Holder,
    Editor,
}

fn stream(word: &str) -> Result<Stream, String> {
    match word {
        "stdout" => Ok(Stream::Stdout),
        "stderr" => Ok(Stream::Stderr),
        other => Err(format!("unknown stream `{other}`")),
    }
}

/// Parses a mode string; an unknown mode or a bad number is an error that names it.
pub fn parse_mode(text: &str) -> Result<Mode, String> {
    let parts: Vec<&str> = text.split(':').collect();
    let number = |word: &str| {
        word.parse::<usize>()
            .map_err(|_| format!("bad number `{word}` in fixture mode `{text}`"))
    };
    match parts.as_slice() {
        ["exit", code] => code
            .parse::<i32>()
            .map(Mode::Exit)
            .map_err(|_| format!("bad exit code `{code}` in fixture mode `{text}`")),
        ["output", which, bytes] => Ok(Mode::Output(stream(which)?, number(bytes)?)),
        ["invalid", which] => Ok(Mode::Invalid(stream(which)?)),
        ["hang"] => Ok(Mode::Hang),
        ["descendants", count] => Ok(Mode::Descendants(number(count)?)),
        ["holder"] => Ok(Mode::Holder),
        ["editor"] => Ok(Mode::Editor),
        _ => Err(format!("unknown fixture mode `{text}`")),
    }
}

fn write_stream(which: Stream, bytes: &[u8]) -> io::Result<()> {
    match which {
        Stream::Stdout => {
            let mut out = io::stdout().lock();
            out.write_all(bytes)?;
            out.flush()
        }
        Stream::Stderr => {
            let mut err = io::stderr().lock();
            err.write_all(bytes)?;
            err.flush()
        }
    }
}

fn target() -> Result<String, String> {
    std::env::var(TARGET_ENV).map_err(|_| format!("{TARGET_ENV} is not set"))
}

/// The command for one hanging copy of this child, with the streams of this process.
fn hanger_command() -> Result<Command, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut command = Command::new(exe);
    command
        .args(["--exact", &entry_selector(), "--nocapture"])
        .env(MODE_ENV, "hang")
        .stdin(Stdio::null());
    Ok(command)
}

/// Starts one hanging copy of this child that shares the streams, and returns its process ID.
fn start_hanger() -> Result<u32, String> {
    let child = hanger_command()?
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    Ok(child.id())
}

/// What the `holder` mode reports: the process that holds the streams, the reporting child's own
/// process ID and, where the streams were lent, the handle values they have in the holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub pid: u32,
    /// The child that wrote the report. It ends right after, so a reader that waits for it to end
    /// knows the child is out of the way.
    pub from: u32,
    pub handles: Vec<usize>,
}

/// The report as the file holds it: the holder's process ID, the reporter's, one line per lent
/// handle and the line `ready`.
fn held_text(held: &Held) -> String {
    let mut text = format!("{}\n{}\n", held.pid, held.from);
    for handle in &held.handles {
        text.push_str(&format!("{handle}\n"));
    }
    text.push_str("ready\n");
    text
}

/// Reads a report. `None` until the text is complete (it ends in the line `ready`) and well
/// formed, so a reader that polls the file never acts on half of a write.
pub fn parse_held(text: &str) -> Option<Held> {
    let body = text.strip_suffix("ready\n")?;
    let mut numbers = body.lines().map(str::parse::<usize>);
    let pid = u32::try_from(numbers.next()?.ok()?).ok()?;
    let from = u32::try_from(numbers.next()?.ok()?).ok()?;
    let handles = numbers.collect::<Result<Vec<_>, _>>().ok()?;
    Some(Held { pid, from, handles })
}

/// Starts a hanging copy in a process group of its own, so the bounded runner's group kill does
/// not reach it.
#[cfg(unix)]
fn start_in_own_group() -> Result<Held, String> {
    use std::os::unix::process::CommandExt;
    let child = hanger_command()?
        .process_group(0)
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    Ok(Held {
        pid: child.id(),
        from: std::process::id(),
        handles: Vec::new(),
    })
}

#[cfg(not(unix))]
fn start_in_own_group() -> Result<Held, String> {
    Err(format!(
        "this platform has no process group to leave: start an independent process and name it in {HOLDER_ENV}"
    ))
}

/// Gives the process `pid`, which the caller owns and which is outside every job this process is
/// in, a copy of both of this process's streams. The copies stay open there until it ends, and
/// the report names them.
#[cfg(windows)]
#[allow(unsafe_code)]
fn lend_streams(pid: u32) -> Result<Held, String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{
        DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE,
    };
    // SAFETY: the call takes no pointer.
    let raw = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, pid) };
    if raw.is_null() {
        return Err(format!(
            "open process {pid}: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: the call succeeded, so `raw` is a new handle that nothing else owns.
    let holder = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut handles = Vec::new();
    for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: the call takes no pointer.
        let own = unsafe { GetStdHandle(which) };
        if own.is_null() || own == INVALID_HANDLE_VALUE {
            return Err(format!("no standard stream {which}"));
        }
        let mut lent: HANDLE = null_mut();
        // SAFETY: the stream and the holder are open for the call, and `lent` outlives it.
        let done = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                own,
                holder.as_raw_handle(),
                &raw mut lent,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if done == 0 {
            return Err(format!("lend a stream: {}", io::Error::last_os_error()));
        }
        handles.push(lent as usize);
    }
    Ok(Held {
        pid,
        from: std::process::id(),
        handles,
    })
}

#[cfg(not(windows))]
fn lend_streams(_pid: u32) -> Result<Held, String> {
    Err(format!(
        "{HOLDER_ENV} is for Windows; Unix starts its own holder"
    ))
}

/// Whether process `held.pid` still has every handle `held` names, after the run that lent them
/// ended. A handle of another process is looked at by copying it into this one, which works only
/// while the other process holds it; the copy is closed again.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn holder_keeps(held: &Held) -> Result<(), String> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE,
    };
    // SAFETY: the call takes no pointer.
    let raw = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, held.pid) };
    if raw.is_null() {
        return Err(format!(
            "open process {}: {}",
            held.pid,
            io::Error::last_os_error()
        ));
    }
    // SAFETY: the call succeeded, so `raw` is a new handle that nothing else owns.
    let holder = unsafe { OwnedHandle::from_raw_handle(raw) };
    for handle in &held.handles {
        let mut copy: HANDLE = null_mut();
        // SAFETY: the holder is open for the call, `copy` outlives it, and the source handle is
        // only named, never used in this process.
        let done = unsafe {
            DuplicateHandle(
                std::os::windows::io::AsRawHandle::as_raw_handle(&holder),
                *handle as HANDLE,
                GetCurrentProcess(),
                &raw mut copy,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if done == 0 {
            return Err(format!(
                "process {} no longer holds handle {handle}: {}",
                held.pid,
                io::Error::last_os_error()
            ));
        }
        // SAFETY: the copy is a new handle that nothing else owns.
        drop(unsafe { OwnedHandle::from_raw_handle(copy) });
    }
    Ok(())
}

/// Runs `mode` in this process. Returns the exit code the process should end with; a hanging
/// mode never returns.
pub fn run_mode(mode: &Mode) -> Result<i32, String> {
    match mode {
        Mode::Exit(code) => Ok(*code),
        Mode::Output(which, bytes) => {
            let chunk = [b'x'; 8192];
            let mut left = *bytes;
            while left > 0 {
                let n = left.min(chunk.len());
                if write_stream(*which, &chunk[..n]).is_err() {
                    // the reader closed the stream: the cap did its work
                    return Ok(0);
                }
                left -= n;
            }
            Ok(0)
        }
        Mode::Invalid(which) => {
            let _ = write_stream(*which, INVALID_BYTES);
            Ok(0)
        }
        Mode::Hang => loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        },
        Mode::Descendants(count) => {
            let mut ids = String::new();
            for _ in 0..*count {
                ids.push_str(&format!("{}\n", start_hanger()?));
            }
            std::fs::write(target()?, ids).map_err(|e| format!("write ids: {e}"))?;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        Mode::Holder => {
            let held = match std::env::var(HOLDER_ENV) {
                Ok(text) => {
                    let pid = text
                        .parse::<u32>()
                        .map_err(|_| format!("bad process ID `{text}` in {HOLDER_ENV}"))?;
                    lend_streams(pid)?
                }
                Err(_) => start_in_own_group()?,
            };
            // written last: the report is also the signal that the streams are held
            std::fs::write(target()?, held_text(&held))
                .map_err(|e| format!("write report: {e}"))?;
            Ok(0)
        }
        Mode::Editor => {
            let text = std::env::var(TEXT_ENV).map_err(|_| format!("{TEXT_ENV} is not set"))?;
            std::fs::write(target()?, text).map_err(|e| format!("write: {e}"))?;
            Ok(0)
        }
    }
}

/// The entry a fixture child runs. With [`MODE_ENV`] unset (an ordinary test run) it does
/// nothing. With it set, it runs the mode and ends the process, so no other test runs in the
/// child.
#[test]
fn fixture_entry() {
    let Ok(text) = std::env::var(MODE_ENV) else {
        return;
    };
    let code = match parse_mode(&text).and_then(|mode| run_mode(&mode)) {
        Ok(code) => code,
        Err(message) => {
            let _ = write_stream(
                Stream::Stderr,
                format!("child fixture: {message}\n").as_bytes(),
            );
            101
        }
    };
    std::process::exit(code);
}

/// A command that starts this test executable as a fixture child in `mode`. The caller adds
/// `TARGET_ENV` or `TEXT_ENV` where the mode needs them.
pub(crate) fn fixture_command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("the test executable"));
    command
        .args(["--exact", &entry_selector(), "--nocapture"])
        .env(MODE_ENV, mode);
    command
}

/// The same command, with `TARGET_ENV` set to `target`.
pub(crate) fn fixture_command_for(mode: &str, target: &Path) -> Command {
    let mut command = fixture_command(mode);
    command.env(TARGET_ENV, target);
    command
}
