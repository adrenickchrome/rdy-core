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
//! - `holder`: the escaped pipe holder. Start one hanging copy that shares the streams, write its
//!   process ID to the file named by [`TARGET_ENV`] and exit 0 at once.
//! - `editor`: write [`TEXT_ENV`] to the file named by [`TARGET_ENV`], as an editor does, and
//!   exit 0.
//!
//! libtest prints its own header to stdout before a test starts, so a mode that needs exact bytes
//! writes them to stderr, or the caller accounts for the header.

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

/// The libtest name that `--exact` selects.
pub(crate) const ENTRY_SELECTOR: &str = "platform::child_fixture::fixture_entry";

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

/// Starts one hanging copy of this child that shares the streams, and returns its process ID.
fn start_hanger() -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut command = Command::new(exe);
    command
        .args(["--exact", ENTRY_SELECTOR, "--nocapture"])
        .env(MODE_ENV, "hang")
        .stdin(Stdio::null());
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
                ids.push_str(&format!("{}\n", start_hanger()?));
            }
            std::fs::write(target()?, ids).map_err(|e| format!("write ids: {e}"))?;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        Mode::Holder => {
            let id = start_hanger()?;
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
        .args(["--exact", ENTRY_SELECTOR, "--nocapture"])
        .env(MODE_ENV, mode);
    command
}

/// The same command, with `TARGET_ENV` set to `target`.
pub(crate) fn fixture_command_for(mode: &str, target: &Path) -> Command {
    let mut command = fixture_command(mode);
    command.env(TARGET_ENV, target);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{kill_hard, process_exited, process_running, run_bounded};
    use std::time::Duration;

    fn stdout_without_header(bytes: &[u8]) -> &[u8] {
        // libtest prints "\nrunning 1 test\n" first; the mode's bytes follow it
        let header = b"\nrunning 1 test\n";
        bytes.strip_prefix(header.as_slice()).unwrap_or(bytes)
    }

    fn ids(path: &Path) -> Vec<u32> {
        std::fs::read_to_string(path)
            .expect("the id file")
            .lines()
            .map(|line| line.parse().expect("an id"))
            .collect()
    }

    #[test]
    fn wx23c_native_child_modes_are_deterministic() {
        // the parser is closed: a bad mode is an error that names it
        assert_eq!(parse_mode("exit:7"), Ok(Mode::Exit(7)));
        assert_eq!(
            parse_mode("output:stderr:12"),
            Ok(Mode::Output(Stream::Stderr, 12))
        );
        assert!(parse_mode("output:tty:1").unwrap_err().contains("tty"));
        assert!(parse_mode("hold").unwrap_err().contains("hold"));
        assert!(parse_mode("exit:x").unwrap_err().contains("exit code"));

        // exit codes
        for code in [0, 3, 101] {
            let status = fixture_command(&format!("exit:{code}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("run the exit fixture");
            assert_eq!(status.code(), Some(code));
        }

        // a bad mode ends 101 and says why on stderr
        let out = fixture_command("nonsense").output().expect("run");
        assert_eq!(out.status.code(), Some(101));
        assert!(String::from_utf8_lossy(&out.stderr).contains("unknown fixture mode `nonsense`"));

        // capped output: the exact count, on the chosen stream, the same on every run
        for _ in 0..2 {
            let out = fixture_command("output:stderr:20000")
                .output()
                .expect("run");
            assert_eq!(out.status.code(), Some(0));
            assert_eq!(out.stderr, vec![b'x'; 20000]);
            let out = fixture_command("output:stdout:5").output().expect("run");
            assert_eq!(stdout_without_header(&out.stdout)[..5], *b"xxxxx");
        }

        // the bounded runner caps the same output
        let err = run_bounded(
            fixture_command("output:stderr:200000"),
            None,
            1000,
            Duration::from_secs(60),
        )
        .expect_err("the cap trips");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        // raw invalid bytes arrive unchanged
        let out = fixture_command("invalid:stderr").output().expect("run");
        assert_eq!(out.stderr, INVALID_BYTES);
        assert!(String::from_utf8(out.stderr).is_err());

        // an editor write
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("edited.txt");
        let status = fixture_command_for("editor", &file)
            .env(TEXT_ENV, "title: edited\n")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run the editor fixture");
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "title: edited\n");

        // a hang ends only when it is killed
        let mut hanging = fixture_command("hang")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the hang fixture");
        assert!(hanging.try_wait().expect("poll").is_none());
        hanging.kill().expect("kill");
        hanging.wait().expect("reap");

        // descendants: two live copies, killed one by one
        let list = dir.path().join("pids");
        let mut parent = fixture_command_for("descendants:2", &list)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the descendants fixture");
        // the file appears once both copies run; wait on its content, not on time
        let pids = loop {
            if let Ok(text) = std::fs::read_to_string(&list)
                && text.lines().count() == 2
            {
                break ids(&list);
            }
            assert!(
                parent.try_wait().expect("poll").is_none(),
                "the parent died"
            );
            std::thread::yield_now();
        };
        assert_ne!(pids[0], pids[1]);
        for pid in &pids {
            assert!(process_running(*pid));
        }
        parent.kill().expect("kill the parent");
        parent.wait().expect("reap");
        for pid in pids {
            kill_hard(pid);
            while !process_exited(pid) {
                std::thread::yield_now();
            }
        }
    }

    #[test]
    fn wx23c_escaped_holder_does_not_hold_the_bounded_run() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("holder");
        let out = run_bounded(
            fixture_command_for("holder", &file),
            None,
            1 << 20,
            Duration::from_secs(60),
        )
        .expect("the run ends when the direct child exits");
        assert_eq!(out.status.code(), Some(0));
        let pids = ids(&file);
        assert_eq!(pids.len(), 1);
        // the runner stopped the holder with the tree
        let pid = pids[0];
        let mut tries = 0;
        while !process_exited(pid) && tries < 1_000_000 {
            std::thread::yield_now();
            tries += 1;
        }
        if !process_exited(pid) {
            kill_hard(pid);
            panic!("the holder survived the run");
        }
    }

    #[test]
    fn wx23c_fixture_is_absent_from_product_release() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read_to_string(root.join("src/platform/child_fixture.rs"))
            .expect("the fixture source");
        // the module is registered only in a test build
        let platform = std::fs::read_to_string(root.join("src/platform.rs")).expect("platform.rs");
        let at = platform
            .find("mod child_fixture;")
            .expect("the registration");
        let lines: Vec<&str> = platform[..at].lines().collect();
        assert!(
            lines
                .last()
                .is_some_and(|line| line.contains("#[cfg(test)]")),
            "mod child_fixture is not behind cfg(test)"
        );
        assert!(source.contains("fn fixture_entry()"));
        // no product binary names the fixture, and the crate adds no binary target
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("manifest");
        assert!(!manifest.contains("[[bin]]"));
        for entry in std::fs::read_dir(root.join("src/bin")).expect("bins") {
            let path = entry.expect("entry").path();
            let text = std::fs::read_to_string(&path).expect("bin source");
            assert!(
                !text.contains("child_fixture") && !text.contains(MODE_ENV),
                "{} names the fixture",
                path.display()
            );
        }
        // the test selector in the source and the constant agree
        assert!(source.contains("fn fixture_entry()"));
        assert_eq!(ENTRY_SELECTOR, "platform::child_fixture::fixture_entry");
    }
}
