//! Tests of the child fixtures (23c). They live beside the fixture, not in it, because the fixture
//! file is also included by the QA integration binaries that reuse it.

use super::child_fixture::*;
use crate::platform::{kill_hard, process_exited, process_running, run_bounded};
use std::io;
use std::path::Path;
use std::process::Stdio;
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
    let deadline = Duration::from_secs(2);
    let started = std::time::Instant::now();
    let out = run_bounded(
        fixture_command_for("holder", &file),
        None,
        1 << 20,
        deadline,
    );
    let took = started.elapsed();
    // the holder id file is written before the direct child exits, so it is ready here
    let pids = ids(&file);
    assert_eq!(pids.len(), 1);
    let pid = pids[0];
    // look first, clean up always: the holder must not outlive this test
    let alive = process_running(pid);
    if alive {
        kill_hard(pid);
    }
    while !process_exited(pid) {
        std::thread::yield_now();
    }
    // the holder keeps the streams open outside the runner's tree: the runner cannot reach it,
    // so the deadline ends the run, and it ends it in bounded time
    let err = out.expect_err("a stream held outside the tree ends the run at its deadline");
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(
        alive,
        "the runner's cleanup stopped the holder: it did not escape the tree"
    );
    assert!(
        took >= deadline && took < Duration::from_secs(30),
        "the run was not bounded by its deadline ({took:?})"
    );
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
