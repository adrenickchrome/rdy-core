#![cfg(unix)]
// Unix only: it reads permission bits under a umask and synchronises on named pipes.
//! The package tool's private stage under any umask (Unix). Windows has no umask: its stage is
//! private by an ACL, which the store's directory calls make and their own tests read.
use super::release_package::{flags, native_or_refused, tar_available_or_skip};
use rdy_qa::fixtures::output_within;
use rdy_qa::platform;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Makes a named pipe at `path`.
fn fifo(path: &Path) {
    let made = Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo {}", path.display());
}

#[test]
fn pre1_package_stage_stays_private_under_any_umask() {
    if !tar_available_or_skip(&rdy_qa::test_name!(
        "pre1_package_stage_stays_private_under_any_umask"
    )) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(target) =
        native_or_refused(dir.path().join("rdy").as_os_str(), &dir.path().join("out"))
    else {
        return;
    };
    let (ready, go) = (dir.path().join("ready"), dir.path().join("go"));
    fifo(&ready);
    fifo(&go);
    // The binary's --version waits on the pipes, so the stage can be inspected mid-probe.
    let bin = dir.path().join("rdy");
    fs::write(
        &bin,
        format!(
            "#!/bin/sh\necho ready > '{}'\nread go < '{}'\nprintf 'rdy 0.14.0\\n'\n",
            ready.display(),
            go.display()
        ),
    )
    .expect("binary");
    let out = dir.path().join("out");
    let mut cmd =
        rdy_store::pio::under_umask("000", Path::new(env!("CARGO_BIN_EXE_qa-release-package")));
    cmd.args(flags(bin.as_os_str(), "0.14.0", target, out.as_os_str()));
    let cli = std::thread::spawn(move || output_within(&mut cmd, Duration::from_secs(600)));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut line = String::new();
        let read =
            fs::File::open(&ready).and_then(|f| std::io::BufReader::new(f).read_line(&mut line));
        let _ = tx.send(read.map(|_| line));
    });
    let line = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the version probe started")
        .expect("read the readiness pipe");
    assert_eq!(line, "ready\n");
    let out_mode = platform::mode_of(&out).expect("OUT's mode");
    let inside: Vec<(String, u32)> = fs::read_dir(&out)
        .expect("read OUT")
        .map(|e| {
            let e = e.expect("entry");
            (
                e.file_name().to_string_lossy().into_owned(),
                platform::mode_of(&e.path()).expect("mode"),
            )
        })
        .collect();
    {
        use std::io::Write;
        fs::File::options()
            .write(true)
            .open(&go)
            .and_then(|mut g| g.write_all(b"go\n"))
            .expect("let the probe finish");
    }
    let done = cli
        .join()
        .expect("the command's thread")
        .expect("run qa-release-package");
    assert_eq!(out_mode, 0o700, "OUT is private under umask 000");
    assert_eq!(inside.len(), 1, "OUT holds only the stage: {inside:?}");
    assert_eq!(inside[0].1, 0o700, "the stage is private under umask 000");
    assert_eq!(done.status.code(), Some(0), "{done:?}");
    let archive = out.join(format!("rdy-0.14.0-{target}.tar.gz"));
    assert_eq!(
        String::from_utf8_lossy(&done.stdout),
        format!("{}\n", archive.display())
    );
    assert_eq!(
        platform::mode_of(&archive).expect("the archive's mode"),
        0o644,
        "the archive is not writable by others under umask 000"
    );
    assert_eq!(fs::read_dir(&out).expect("read_dir").count(), 1);
}
