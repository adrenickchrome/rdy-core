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
//! - `script`: a scripted stdio peer, for the MCP stubs (see [`Step`]). The steps are the lines
//!   of [`SCRIPT_ENV`]. They replace every shell stub of the MCP area: the same bytes run on
//!   every platform and no shell, `cat`, `head`, `tr` or `sleep` is needed.
//!
//! - `edit`: a scripted editor (see [`EditStep`]). It edits the file that is the last argument on
//!   its command line, as an editor does, so `rdy edit` starts it with the buffer path as its own
//!   argument on every platform. The steps are the lines of [`EDIT_ENV`]. They replace every
//!   shell editor script of the CLI and brief areas.
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
/// The environment variable that holds the steps of the `script` mode, one per line.
pub const SCRIPT_ENV: &str = "RDY_QA_CHILD_SCRIPT";
/// The environment variable that holds the steps of the `edit` mode, one per line.
pub const EDIT_ENV: &str = "RDY_QA_CHILD_EDIT";
/// The environment variable that holds the steps the `nest` step of the `edit` mode gives to the
/// editor it starts inside the first one.
pub const INNER_ENV: &str = "RDY_QA_CHILD_INNER";
/// The environment variable that names the product executable the `nest` and `stop` steps run.
pub const RDY_ENV: &str = "RDY_QA_CHILD_RDY";
/// The environment variable that names the project those steps run against.
pub const PROJECT_ENV: &str = "RDY_QA_CHILD_PROJECT";
/// The first bytes libtest writes to stdout before it runs the entry test. A reader of a
/// fixture's stdout that must see only the fixture's own bytes drops exactly these.
pub const BANNER: &[u8] = b"\nrunning 1 test\n";
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
    Script,
    Edit,
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
        ["script"] => Ok(Mode::Script),
        ["edit"] => Ok(Mode::Edit),
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

/// One step of the `script` mode. A script is lines; a line is a word, then (for some words) a
/// space and the rest of the line, taken as it is.
///
/// - `read`: read one line of stdin into the last line. The end of stdin ends the process, exit 0.
/// - `say TEXT`: write `TEXT` and LF to stdout. `put TEXT`: the same with no LF.
/// - `rawout HEX` and `rawerr HEX`: write those bytes (hex pairs) to stdout or stderr.
/// - `fill out|err COUNT HEXBYTE`: write `COUNT` copies of the byte.
/// - `drain`: read stdin to its end.
/// - `exit CODE`, `hang`.
/// - `closein`: close the process's own stdin, so the writer of the pipe sees it broken.
/// - `bghang`: start a hanging copy that shares the streams; its process ID is the last line.
/// - `note TEXT`: write `TEXT` and LF to the file named by [`TARGET_ENV`], replacing it.
/// - `record`: write the last line and LF to that file, replacing it. `append`: add them.
/// - `loopsay TEXT`: say `TEXT`, read a line, and again, until stdin ends.
/// - `askforever`: for each line read, say an `input_required` reply to the call with the number of
///   lines read so far as its id and `h<number>` as its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Read,
    Say(String),
    Put(String),
    Raw(Stream, Vec<u8>),
    Fill(Stream, usize, u8),
    Drain,
    Exit(i32),
    Hang,
    CloseIn,
    BgHang,
    Note(String),
    Record,
    Append,
    LoopSay(String),
    AskForever,
}

fn hex_bytes(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err(format!("bad hex `{text}`"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| format!("bad hex `{text}`")))
        .collect()
}

fn out_err(word: &str) -> Result<Stream, String> {
    match word {
        "out" => Ok(Stream::Stdout),
        "err" => Ok(Stream::Stderr),
        other => Err(format!("unknown stream `{other}`")),
    }
}

/// Parses a script; a line that is not a step is an error that names it.
pub fn parse_script(text: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let step = match word {
            "read" => Step::Read,
            "say" => Step::Say(rest.to_string()),
            "put" => Step::Put(rest.to_string()),
            "rawout" => Step::Raw(Stream::Stdout, hex_bytes(rest)?),
            "rawerr" => Step::Raw(Stream::Stderr, hex_bytes(rest)?),
            "fill" => {
                let parts: Vec<&str> = rest.split(' ').collect();
                let [which, count, byte] = parts.as_slice() else {
                    return Err(format!("bad fill step `{line}`"));
                };
                let count = count
                    .parse::<usize>()
                    .map_err(|_| format!("bad number in `{line}`"))?;
                let byte = hex_bytes(byte)?;
                let [byte] = byte.as_slice() else {
                    return Err(format!("bad byte in `{line}`"));
                };
                Step::Fill(out_err(which)?, count, *byte)
            }
            "drain" => Step::Drain,
            "exit" => Step::Exit(
                rest.parse::<i32>()
                    .map_err(|_| format!("bad exit code in `{line}`"))?,
            ),
            "hang" => Step::Hang,
            "closein" => Step::CloseIn,
            "bghang" => Step::BgHang,
            "note" => Step::Note(rest.to_string()),
            "record" => Step::Record,
            "append" => Step::Append,
            "loopsay" => Step::LoopSay(rest.to_string()),
            "askforever" => Step::AskForever,
            _ => return Err(format!("unknown script step `{line}`")),
        };
        steps.push(step);
    }
    Ok(steps)
}

/// Reads one line of stdin, without its LF; `None` at the end of stdin.
fn read_input_line() -> Option<String> {
    use std::io::BufRead;
    let mut raw = Vec::new();
    match io::stdin().lock().read_until(b'\n', &mut raw) {
        Ok(0) | Err(_) => None,
        Ok(_) => {
            if raw.last() == Some(&b'\n') {
                raw.pop();
            }
            Some(String::from_utf8_lossy(&raw).into_owned())
        }
    }
}

/// Closes this process's standard input.
#[cfg(unix)]
#[allow(unsafe_code)]
fn close_input() {
    use std::os::fd::{FromRawFd, OwnedFd};
    // SAFETY: descriptor 0 is the process's own stdin; nothing here reads it afterwards.
    drop(unsafe { OwnedFd::from_raw_fd(0) });
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn close_input() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
    // SAFETY: the handle is the process's own stdin; nothing here reads it afterwards.
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        if !handle.is_null() {
            CloseHandle(handle);
        }
    }
}

fn write_target(text: &str, append: bool) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(target()?)
        .map_err(|e| format!("open target: {e}"))?;
    file.write_all(text.as_bytes())
        .and_then(|_| file.flush())
        .map_err(|e| format!("write target: {e}"))
}

fn forever() -> ! {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// Runs the steps. The end of the script is exit 0. A closed stdout or stderr ends the process
/// the way a closed pipe ends a shell peer: exit 0.
fn run_script(steps: &[Step]) -> Result<i32, String> {
    let mut last = String::new();
    let mut lines = 0usize;
    for step in steps {
        let wrote = match step {
            Step::Read => match read_input_line() {
                Some(line) => {
                    last = line;
                    lines += 1;
                    Ok(())
                }
                None => return Ok(0),
            },
            Step::Say(text) => write_stream(Stream::Stdout, format!("{text}\n").as_bytes()),
            Step::Put(text) => write_stream(Stream::Stdout, text.as_bytes()),
            Step::Raw(which, bytes) => write_stream(*which, bytes),
            Step::Fill(which, count, byte) => {
                let chunk = vec![*byte; 8192];
                let mut left = *count;
                let mut result = Ok(());
                while left > 0 && result.is_ok() {
                    let n = left.min(chunk.len());
                    result = write_stream(*which, &chunk[..n]);
                    left -= n;
                }
                result
            }
            Step::Drain => {
                let _ = io::copy(&mut io::stdin().lock(), &mut io::sink());
                Ok(())
            }
            Step::Exit(code) => return Ok(*code),
            Step::Hang => forever(),
            Step::CloseIn => {
                close_input();
                Ok(())
            }
            Step::BgHang => {
                // its streams are nobody's: its own libtest banner must not reach the reader
                let child = hanger_command()?
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| format!("spawn: {e}"))?;
                last = child.id().to_string();
                Ok(())
            }
            Step::Note(text) => {
                write_target(&format!("{text}\n"), false)?;
                Ok(())
            }
            Step::Record => {
                write_target(&format!("{last}\n"), false)?;
                Ok(())
            }
            Step::Append => {
                write_target(&format!("{last}\n"), true)?;
                Ok(())
            }
            Step::LoopSay(text) => loop {
                if write_stream(Stream::Stdout, format!("{text}\n").as_bytes()).is_err() {
                    return Ok(0);
                }
                if read_input_line().is_none() {
                    return Ok(0);
                }
            },
            Step::AskForever => {
                while read_input_line().is_some() {
                    lines += 1;
                    let reply = format!(
                        r#"{{"jsonrpc":"2.0","id":{lines},"result":{{"resultType":"input_required","requestState":"h{lines}","inputRequests":{{"answer":{{"method":"elicitation/create","params":{{"mode":"form","message":"m","requestedSchema":{{"type":"object"}}}}}}}}}}}}"#
                    );
                    if write_stream(Stream::Stdout, format!("{reply}\n").as_bytes()).is_err() {
                        return Ok(0);
                    }
                }
                return Ok(0);
            }
        };
        if wrote.is_err() {
            // the reader closed the stream: the cap or a stop did its work
            return Ok(0);
        }
    }
    Ok(0)
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
        Mode::Script => {
            let text = std::env::var(SCRIPT_ENV).map_err(|_| format!("{SCRIPT_ENV} is not set"))?;
            run_script(&parse_script(&text)?)
        }
        Mode::Edit => {
            let text = std::env::var(EDIT_ENV).map_err(|_| format!("{EDIT_ENV} is not set"))?;
            let buffer = std::env::args_os()
                .last()
                .map(std::path::PathBuf::from)
                .ok_or("no buffer path on the command line")?;
            run_edit(&parse_edit(&text)?, &buffer)
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
        // a single test thread would make libtest print the test's name before it runs
        .env_remove("RUST_TEST_THREADS")
        .env(MODE_ENV, mode);
    command
}

/// A scripted stdio peer (the `script` mode). `arg` is the file the steps `note`, `record` and
/// `append` write to.
pub(crate) fn script_command(script: &str, arg: Option<&Path>) -> Command {
    let mut command = fixture_command("script");
    command.env(SCRIPT_ENV, script);
    if let Some(arg) = arg {
        command.env(TARGET_ENV, arg);
    }
    command
}

/// The same command, with `TARGET_ENV` set to `target`.
pub(crate) fn fixture_command_for(mode: &str, target: &Path) -> Command {
    let mut command = fixture_command(mode);
    command.env(TARGET_ENV, target);
    command
}

/// One step of the `edit` mode. A script is lines; a line is a word, then a space and the rest of
/// the line, taken as it is. Where a step has several parts they are separated by a tab. In a
/// TEXT part the two characters `\n` stand for a line feed; a path is taken as it is.
///
/// - `copy PATH`: write the buffer as it is now to `PATH`.
/// - `replace FROM<tab>TO`: replace every `FROM` in the buffer with `TO`.
/// - `swapline FROM<tab>TO`: a line that is `FROM` after its indentation becomes `TO`, with the
///   same indentation.
/// - `after PREFIX<tab>TEXT`: after each line that starts with `PREFIX`, add the line `TEXT`.
/// - `append TEXT`: add `TEXT` and a line feed at the end. `write TEXT`: the buffer becomes
///   `TEXT` and a line feed.
/// - `nest ID`: run `rdy --project P edit ID`, with this editor and the steps of [`INNER_ENV`], and
///   wait for it. Its output is dropped. `P` is [`PROJECT_ENV`] and `rdy` is [`RDY_ENV`].
/// - `stop OUT<tab>ERR<tab>NOTICE`: run `rdy --project P serve --stop` with stdout in the file
///   `OUT` and stderr in the file `ERR`, behind the line `NOTICE` there when it is not empty;
///   then add the line `exit=CODE` to `OUT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditStep {
    Copy(String),
    Replace(String, String),
    SwapLine(String, String),
    After(String, String),
    Append(String),
    Write(String),
    Nest(String),
    Stop(String, String, String),
}

fn edit_text(text: &str) -> String {
    text.replace("\\n", "\n")
}

fn parts<const N: usize>(rest: &str, line: &str) -> Result<[String; N], String> {
    let found: Vec<String> = rest.splitn(N, '\t').map(str::to_string).collect();
    found
        .try_into()
        .map_err(|_| format!("bad edit step `{line}`"))
}

/// Parses an edit script; a line that is not a step is an error that names it.
pub fn parse_edit(text: &str) -> Result<Vec<EditStep>, String> {
    let mut steps = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let step = match word {
            "copy" => EditStep::Copy(rest.to_string()),
            "replace" => {
                let [from, to] = parts(rest, line)?;
                EditStep::Replace(from, to)
            }
            "swapline" => {
                let [from, to] = parts(rest, line)?;
                EditStep::SwapLine(from, to)
            }
            "after" => {
                let [prefix, add] = parts(rest, line)?;
                EditStep::After(prefix, edit_text(&add))
            }
            "append" => EditStep::Append(edit_text(rest)),
            "write" => EditStep::Write(edit_text(rest)),
            "nest" => EditStep::Nest(rest.to_string()),
            "stop" => {
                let [out, err, notice] = parts(rest, line)?;
                EditStep::Stop(out, err, notice)
            }
            _ => return Err(format!("unknown edit step `{line}`")),
        };
        steps.push(step);
    }
    Ok(steps)
}

fn env_path(name: &str) -> Result<std::path::PathBuf, String> {
    std::env::var_os(name)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| format!("{name} is not set"))
}

fn run_nested(id: &str) -> Result<(), String> {
    let inner = std::env::var(INNER_ENV).map_err(|_| format!("{INNER_ENV} is not set"))?;
    let own = std::env::var_os("EDITOR").ok_or("EDITOR is not set")?;
    Command::new(env_path(RDY_ENV)?)
        .arg("--project")
        .arg(env_path(PROJECT_ENV)?)
        .args(["edit", id])
        .env("EDITOR", own)
        .env(EDIT_ENV, inner)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| ())
        .map_err(|e| format!("nested edit: {e}"))
}

fn run_stop(out: &str, err: &str, notice: &str) -> Result<(), String> {
    let mut errf = std::fs::File::create(err).map_err(|e| format!("create {err}: {e}"))?;
    if !notice.is_empty() {
        writeln!(errf, "{notice}").map_err(|e| format!("write {err}: {e}"))?;
    }
    let outf = std::fs::File::create(out).map_err(|e| format!("create {out}: {e}"))?;
    let status = Command::new(env_path(RDY_ENV)?)
        .arg("--project")
        .arg(env_path(PROJECT_ENV)?)
        .args(["serve", "--stop"])
        .stdin(Stdio::null())
        .stdout(outf)
        .stderr(errf)
        .status()
        .map_err(|e| format!("serve --stop: {e}"))?;
    let mut outf = std::fs::OpenOptions::new()
        .append(true)
        .open(out)
        .map_err(|e| format!("open {out}: {e}"))?;
    writeln!(outf, "exit={}", status.code().unwrap_or(-1)).map_err(|e| format!("write {out}: {e}"))
}

/// Runs the steps on the file `buffer`. The file is written after each step that changes it.
fn run_edit(steps: &[EditStep], buffer: &Path) -> Result<i32, String> {
    let mut text =
        std::fs::read_to_string(buffer).map_err(|e| format!("read {}: {e}", buffer.display()))?;
    for step in steps {
        let before = text.clone();
        match step {
            EditStep::Copy(path) => {
                std::fs::write(path, &text).map_err(|e| format!("copy to {path}: {e}"))?;
            }
            EditStep::Replace(from, to) => text = text.replace(from.as_str(), to),
            EditStep::SwapLine(from, to) => {
                text = text
                    .split_inclusive('\n')
                    .map(|line| {
                        let body = line.trim_end_matches(['\r', '\n']);
                        let rest = body.trim_start();
                        if rest == from {
                            let indent = &body[..body.len() - rest.len()];
                            let end = &line[body.len()..];
                            format!("{indent}{to}{end}")
                        } else {
                            line.to_string()
                        }
                    })
                    .collect();
            }
            EditStep::After(prefix, add) => {
                text = text
                    .split_inclusive('\n')
                    .map(|line| {
                        if line.starts_with(prefix.as_str()) {
                            let end = if line.ends_with('\n') { "" } else { "\n" };
                            format!("{line}{end}{add}\n")
                        } else {
                            line.to_string()
                        }
                    })
                    .collect();
            }
            EditStep::Append(add) => text.push_str(&format!("{add}\n")),
            EditStep::Write(new) => text = format!("{new}\n"),
            EditStep::Nest(id) => run_nested(id)?,
            EditStep::Stop(out, err, notice) => run_stop(out, err, notice)?,
        }
        if text != before {
            std::fs::write(buffer, &text)
                .map_err(|e| format!("write {}: {e}", buffer.display()))?;
        }
    }
    Ok(0)
}

/// The value of `EDITOR` that starts the test executable `exe` as an `edit` fixture. Where the
/// platform splits the value itself (Windows) the program is in double quotes, so a path with
/// spaces works. On Unix `sh` splits the value at white space, so such a path cannot be named.
pub fn editor_value(exe: &Path) -> String {
    let selector = entry_selector();
    if cfg!(windows) {
        format!("\"{}\" --exact {selector} --nocapture", exe.display())
    } else {
        let shown = exe.display().to_string();
        assert!(
            !shown.contains(char::is_whitespace),
            "{shown}: sh splits an editor path at white space"
        );
        format!("{shown} --exact {selector} --nocapture")
    }
}

/// Makes `command` run `script` as its editor: `EDITOR` starts the test executable `exe`, and
/// `VISUAL` is removed so that no person's setting wins on Windows.
pub fn set_editor_at<'a>(command: &'a mut Command, exe: &Path, script: &str) -> &'a mut Command {
    command
        .env_remove("VISUAL")
        .env("EDITOR", editor_value(exe))
        .env(MODE_ENV, "edit")
        .env(EDIT_ENV, script)
}

/// [`set_editor_at`] with this test executable.
pub fn set_editor<'a>(command: &'a mut Command, script: &str) -> &'a mut Command {
    let exe = std::env::current_exe().expect("the test executable");
    set_editor_at(command, &exe, script)
}

/// Names the product executable and project that the `nest` and `stop` steps run, and the steps
/// the nested editor runs.
pub fn set_editor_rdy<'a>(
    command: &'a mut Command,
    rdy: &Path,
    project: &Path,
    inner: Option<&str>,
) -> &'a mut Command {
    command.env(RDY_ENV, rdy).env(PROJECT_ENV, project);
    if let Some(inner) = inner {
        command.env(INNER_ENV, inner);
    }
    command
}

/// `bytes` without the libtest header that an editor child writes to the stdout it shares with
/// the command that started it.
pub fn without_banner(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(BANNER).unwrap_or(bytes)
}

/// Runs `command` to its end and returns its output with the editor child's header dropped.
pub fn edit_output(command: &mut Command) -> io::Result<std::process::Output> {
    let mut out = command.output()?;
    out.stdout = without_banner(&out.stdout).to_vec();
    Ok(out)
}
