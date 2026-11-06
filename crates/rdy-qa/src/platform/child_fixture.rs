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
//! - `holder`: the escaped pipe holder. Start one hanging copy that shares the streams and is
//!   outside the starter's process tree (its own process group, or a job breakaway), write its
//!   process ID to the file named by [`TARGET_ENV`] and exit 0 at once.
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

/// Makes the command's child leave the process tree a bounded runner stops when its direct child
/// ends: on Unix a process group of its own, on Windows a new group that breaks away from the
/// job. A job that forbids breakaway refuses the start, and the caller sees that error.
fn leave_runner_tree(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = command;
}

/// Starts one hanging copy of this child that shares the streams, and returns its process ID.
/// With `escaped`, the copy is outside the process tree of the child that starts it.
fn start_hanger(escaped: bool) -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut command = Command::new(exe);
    command
        .args(["--exact", &entry_selector(), "--nocapture"])
        .env(MODE_ENV, "hang")
        .stdin(Stdio::null());
    if escaped {
        leave_runner_tree(&mut command);
    }
    let child = command.spawn().map_err(|e| format!("spawn: {e}"))?;
    Ok(child.id())
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
                ids.push_str(&format!("{}\n", start_hanger(false)?));
            }
            std::fs::write(target()?, ids).map_err(|e| format!("write ids: {e}"))?;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        Mode::Holder => {
            let id = start_hanger(true)?;
            std::fs::write(target()?, format!("{id}\n")).map_err(|e| format!("write id: {e}"))?;
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
