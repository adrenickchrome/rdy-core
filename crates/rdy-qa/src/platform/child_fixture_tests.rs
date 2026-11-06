//! Tests of the child fixtures (23c). They live beside the fixture, not in it, because the fixture
//! file is also included by the QA integration binaries that reuse it.

use super::child_fixture::*;
use crate::platform::{kill_hard, process_exited, process_running, run_bounded, run_bounded_inner};
use std::io;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

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

/// The holder of the escaped-holder test, and its cleanup. On Windows the holder is a process
/// this test starts, outside the runner's job; on Unix it is the group the fixture child starts.
/// `Drop` ends it whatever the test found, so no holder outlives the test.
#[derive(Default)]
struct Apart {
    child: Option<Child>,
    pid: Option<u32>,
}

impl Apart {
    fn clean_up(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(pid) = self.pid.take() {
            if process_running(pid) {
                kill_hard(pid);
            }
            let guard = Instant::now() + Duration::from_secs(60);
            while !process_exited(pid) && Instant::now() < guard {
                std::thread::yield_now();
            }
        }
    }
}

impl Drop for Apart {
    fn drop(&mut self) {
        self.clean_up();
    }
}

/// The report the holder child writes, read once it says it is complete, and then the end of the
/// child that wrote it. Waits on the file and on the process, under a hang guard that is never a
/// speed check. From here on only the holder can keep a stream of the run open.
fn wait_for_held_and_child_gone(file: &Path) -> Option<Held> {
    let guard = Instant::now() + Duration::from_secs(60);
    while Instant::now() < guard {
        if let Ok(text) = std::fs::read_to_string(file)
            && let Some(held) = parse_held(&text)
        {
            while !process_exited(held.from) && Instant::now() < guard {
                std::thread::yield_now();
            }
            return Some(held);
        }
        std::thread::yield_now();
    }
    None
}

#[test]
fn wx23c_held_report_is_complete_only_when_it_says_ready() {
    assert_eq!(HOLDER_ENV, "RDY_QA_CHILD_HOLDER");
    assert_eq!(parse_held(""), None);
    assert_eq!(parse_held("12\n7\n"), None);
    assert_eq!(parse_held("12\n7\n34\n56\nrea"), None);
    assert_eq!(parse_held("12\nready\n"), None);
    assert_eq!(
        parse_held("12\n7\nready\n"),
        Some(Held {
            pid: 12,
            from: 7,
            handles: vec![]
        })
    );
    assert_eq!(
        parse_held("12\n7\n34\n56\nready\n"),
        Some(Held {
            pid: 12,
            from: 7,
            handles: vec![34, 56]
        })
    );
    assert_eq!(parse_held("x\n7\nready\n"), None);
    assert_eq!(parse_held("12\n7\nx\nready\n"), None);
}

#[cfg(windows)]
#[test]
fn wx23c_holder_needs_an_independent_process_on_windows() {
    // a child inside a job without breakaway cannot start a process outside it: without the
    // independent holder the mode says so and starts nothing
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("holder");
    let out = fixture_command_for("holder", &file)
        .output()
        .expect("run the holder fixture");
    assert_eq!(out.status.code(), Some(101));
    assert!(String::from_utf8_lossy(&out.stderr).contains(HOLDER_ENV));
    assert!(!file.exists());
}

#[test]
fn wx23c_escaped_holder_does_not_hold_the_bounded_run() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("holder");
    let mut apart = Apart::default();
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = fixture_command_for("holder", &file);
    #[cfg(windows)]
    {
        // The runner's job allows no breakaway, so the child cannot start a process outside it.
        // This test starts the holder, outside the job and under its own ownership, before the
        // run. The child lends it both streams, then reports.
        let mut hold = fixture_command("hang");
        hold.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = crate::platform::spawn_apart(&mut hold).expect("start the independent holder");
        command.env(HOLDER_ENV, child.id().to_string());
        apart.child = Some(child);
    }
    // The run has no deadline of its own beyond a hang guard: the expiry comes once the child has
    // reported that the streams are held and has ended, so no timing decides the outcome. What
    // proves the hold is what the holder still has once the run is over, checked below.
    let guard = Instant::now() + Duration::from_secs(120);
    let report = file.clone();
    let (seen_tx, seen) = std::sync::mpsc::channel();
    let run = run_bounded_inner(command, None, None, 1 << 20, Some(guard), move |expiry| {
        std::thread::spawn(move || {
            let _ = seen_tx.send(wait_for_held_and_child_gone(&report));
            expiry.expire();
        });
    });
    let held = seen
        .recv()
        .expect("the waiter reported")
        .expect("the child reported before the guard");
    #[cfg(not(windows))]
    {
        apart.pid = Some(held.pid);
    }
    // look first, clean up always: the holder must not outlive this test
    let alive = process_running(held.pid);
    #[cfg(windows)]
    let keeps = holder_keeps(&held);
    #[cfg(windows)]
    {
        assert_eq!(apart.child.as_ref().map(Child::id), Some(held.pid));
    }
    apart.clean_up();
    assert!(process_exited(held.pid), "the holder was not cleaned up");
    // the holder is outside the runner's tree and keeps the streams open: the runner cannot reach
    // it, so only the expiry ends the run
    let err = run
        .result
        .expect_err("the expiry ends a run whose streams a holder outside the tree keeps");
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert_eq!(run.drains_joined, 2, "both drains were joined");
    assert!(
        process_exited(run.pid.expect("the child started")),
        "the direct child was reaped"
    );
    assert!(
        alive,
        "the runner's cleanup stopped the holder: it did not escape the tree"
    );
    #[cfg(windows)]
    {
        assert_eq!(held.handles.len(), 2);
        keeps.expect("the holder kept the lent streams after the run returned");
    }
    #[cfg(not(windows))]
    assert!(held.handles.is_empty());
}

#[test]
fn wx23c_entry_selector_names_the_module_it_is_in() {
    assert_eq!(entry_selector(), "platform::child_fixture::fixture_entry");
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
    assert_eq!(entry_selector(), "platform::child_fixture::fixture_entry");
}

#[test]
fn wx26b_edit_mode_steps_are_closed_and_edit_the_last_argument() {
    assert_eq!(
        parse_edit("replace a\tb\nafter fields:\t  k: v\\n\nstop o\te\t\n").unwrap(),
        vec![
            EditStep::Replace("a".into(), "b".into()),
            EditStep::After("fields:".into(), "  k: v\n".into()),
            EditStep::Stop("o".into(), "e".into(), String::new()),
        ]
    );
    assert!(parse_edit("sed s/a/b/").unwrap_err().contains("sed"));
    assert!(
        parse_edit("replace only-one")
            .unwrap_err()
            .contains("replace")
    );
    assert_eq!(parse_mode("edit"), Ok(Mode::Edit));

    // the buffer is the last argument, the steps change it in order, and the file keeps its
    // indentation and line ends
    let dir = tempfile::tempdir().unwrap();
    let buffer = dir.path().join("buf");
    std::fs::write(&buffer, "fields:\n  name: d\n  title: Before\n").unwrap();
    let seen = dir.path().join("seen");
    let script = format!(
        "copy {}\nswapline name: d\tname: e\nafter   title: \t  more: 1\nappend end\n",
        seen.display()
    );
    let mut command = fixture_command("edit");
    command.env(EDIT_ENV, script).arg(&buffer);
    let out = command.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        "fields:\n  name: d\n  title: Before\n"
    );
    assert_eq!(
        std::fs::read_to_string(&buffer).unwrap(),
        "fields:\n  name: e\n  title: Before\n  more: 1\nend\n"
    );
    assert_eq!(without_banner(&out.stdout), b"");
}
