// Unix only: package fixtures that need shell scripts, named pipes, sockets, permission modes or a
// directory renamed under a running child. The portable package tests live beside them in the
// parent module (Task 35a); Windows keeps a rename-refusing pin in its own seam.
//! The Unix-only package fixtures of `release_install`.

use super::*;
use crate::platform::test_support::{fifo, fifo_line, unix_socket};
use crate::platform::{Expiry, run_bounded_inner};

#[test]
fn pre1_package_classifies_real_shell_probe_children() {
    // Real children through the runner: status and stderr count, and the runner's own
    // failures take precedence over the version check.
    let probe = |script: &str| {
        classify_probe(
            run_bounded(
                shell_script(script),
                None,
                crate::platform::RELEASE_CHILD_OUTPUT_MAX,
                std::time::Duration::from_secs(600),
            ),
            V,
        )
    };
    assert_eq!(probe("printf 'rdy 0.14.0 (preview)\\n'"), Ok(true));
    assert_eq!(probe("printf 'rdy 0.14.0\\n'"), Ok(false));
    assert_eq!(probe("printf 'rdy 0.15.0\\n'"), Err(Failure::BinaryVersion));
    assert_eq!(
        probe("printf 'rdy 0.14.0\\n'; printf 'warning\\n' >&2"),
        Err(Failure::BinaryVersion)
    );
    assert_eq!(
        probe("printf 'rdy 0.14.0\\n'; exit 1"),
        Err(Failure::BinaryVersion)
    );
    assert_eq!(probe("head -c 1048577 /dev/zero"), Err(Failure::SizeLimit));
    assert_eq!(
        probe("printf 'rdy 0.14.0\\n'; head -c 1048576 /dev/zero >&2"),
        Err(Failure::SizeLimit)
    );
    let run = run_bounded_inner(
        shell_script("printf 'rdy 0.14.0\\n'; exec sleep 1000"),
        None,
        None,
        crate::platform::RELEASE_CHILD_OUTPUT_MAX,
        None,
        Expiry::expire,
    );
    assert_eq!(classify_probe(run.result, V), Err(Failure::FileOperation));
}

#[test]
fn pre1_package_calls_socket_and_locked_fifo_inputs_invalid() {
    let probe = printing("rdy 0.14.0\n");
    // a socket never opens; a FIFO without read access does not open for this user
    let socket: Spoil = |p| {
        fs::remove_file(p).expect("rm");
        drop(unix_socket(p));
    };
    let locked_fifo: Spoil = |p| {
        fs::remove_file(p).expect("rm");
        fifo(p);
        platform::set_mode(p, 0o000).expect("chmod");
    };
    for (what, spoil) in [("a socket", socket), ("a locked fifo", locked_fifo)] {
        for input in ["binary", MIT_FILE, APACHE_FILE] {
            let f = Fixture::new();
            let path = if input == "binary" {
                f.binary.clone()
            } else {
                f.licenses.join(input)
            };
            spoil(&path);
            let before = f.state();
            let got = package_with(&f.binary, V, TARGET, &f.out(), &setup(&f, &probe));
            assert_eq!(got, Err(Failure::InputFile), "{input} as {what}");
            assert_eq!(f.state(), before, "{input} as {what}: nothing changed");
            assert!(!f.out().exists(), "{input} as {what}: no OUT");
        }
    }
}

#[test]
fn pre1_package_refuses_a_fifo_input_and_copies_a_checked_one() {
    // A FIFO is refused at the check, without waiting for a writer.
    let cases: [(&str, Change); 2] = [
        ("a FIFO binary", |f| {
            fs::remove_file(&f.binary).expect("rm");
            fifo(&f.binary);
        }),
        ("a FIFO LICENSE-MIT", |f| {
            let p = f.licenses.join(MIT_FILE);
            fs::remove_file(&p).expect("rm");
            fifo(&p);
        }),
    ];
    for (what, prepare) in cases {
        let (got, f) = package_changed(prepare, |_| {});
        assert_eq!(got, Err(Failure::InputFile), "{what}");
        assert!(!f.out().exists(), "{what}: no OUT");
    }

    // Once checked, a FIFO in place of the binary does not change what is packaged.
    if !tar_or_skip(&crate::test_name!(
        "pre1_package_refuses_a_fifo_input_and_copies_a_checked_one"
    )) {
        return;
    }
    let (got, f) = package_changed(
        |_| {},
        |f| {
            fs::remove_file(&f.binary).expect("rm");
            fifo(&f.binary);
        },
    );
    let archive = got
        .map_err(Failure::message)
        .unwrap_or_else(|e| panic!("a FIFO in place of the binary: {e}"));
    let into = f.dir.path().join("x");
    extract(&archive, &into);
    let root = into.join("rdy-0.14.0-aarch64-apple-darwin");
    assert_eq!(fs::read(root.join("rdy")).expect("rdy"), BINARY);
}

/// A paused child: it writes `ready` to the named pipe `ready`, waits for a line on the named
/// pipe `go`, then runs the shell text `then`.
fn paused(ready: &Path, go: &Path, then: &str) -> Command {
    let mut c = shell_script(&format!("echo ready > \"$0\"; read go < \"$1\"; {then}"));
    c.arg(ready).arg(go);
    c
}

/// On a thread of its own: waits for the ready line, runs `act`, then sends the go line. The
/// returned channel reports that `act` ran.
fn on_ready(
    ready: PathBuf,
    go: PathBuf,
    act: impl FnOnce() + Send + 'static,
) -> std::sync::mpsc::Receiver<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if fifo_line(ready)
            .recv_timeout(std::time::Duration::from_secs(60))
            .is_err()
        {
            return;
        }
        act();
        use std::io::Write;
        let sent = File::options()
            .write(true)
            .open(&go)
            .and_then(|mut g| g.write_all(b"go\n"));
        if sent.is_ok() {
            let _ = tx.send(());
        }
    });
    rx
}

/// A paused wrapper of `real`: it writes `ready` to the named pipe `ready`, waits for a line
/// on the named pipe `go`, then execs `real`'s program with its arguments and environment.
fn paused_exec(ready: &Path, go: &Path, real: &Command) -> Command {
    let mut c = shell_script("echo ready > \"$0\"; read go < \"$1\"; shift; exec \"$@\"");
    c.arg(ready)
        .arg(go)
        .arg(real.get_program())
        .args(real.get_args());
    for (key, value) in real.get_envs() {
        match value {
            Some(value) => c.env(key, value),
            None => c.env_remove(key),
        };
    }
    c
}

#[test]
fn pre1_package_never_takes_an_archive_path_it_did_not_create() {
    const STEM: &str = "rdy-0.14.0-preview-aarch64-apple-darwin";
    if !tar_or_skip(&crate::test_name!(
        "pre1_package_never_takes_an_archive_path_it_did_not_create"
    )) {
        return;
    }
    let missing = |_: &str| Command::new("/nonexistent/rdy-qa-tar");
    for (case, what) in [
        "a newcomer at the archive path before tar, with tar",
        "a newcomer at the archive path before tar, with a tar that cannot start",
        "a newcomer in place of the archive while tar runs, then tar succeeds",
        "a newcomer in place of the archive while tar runs, then tar fails",
        "a newcomer in place of the archive before the real tar starts",
    ]
    .into_iter()
    .enumerate()
    {
        let f = Fixture::new();
        let before = f.state();
        let (ready, go) = (f.dir.path().join("ready"), f.dir.path().join("go"));
        fifo(&ready);
        fifo(&go);
        let archive = f.out().join(format!("{STEM}.tar.gz"));
        let placed = on_ready(ready.clone(), go.clone(), {
            let archive = archive.clone();
            move || {
                let _ = fs::remove_file(&archive);
                fs::write(&archive, b"newcomer\n").expect("place the newcomer");
            }
        });
        let paused_probe = {
            let (r, g) = (ready.clone(), go.clone());
            move |_: &Path| paused(&r, &g, "printf 'rdy 0.14.0 (preview)\\n'")
        };
        let plain_probe = printing("rdy 0.14.0 (preview)\\n");
        let paused_tar = {
            let (r, g) = (ready.clone(), go.clone());
            let code = if case == 2 { 0 } else { 3 };
            move |_: &str| paused(&r, &g, &format!("exit {code}"))
        };
        let paused_real_tar = {
            let (r, g) = (ready.clone(), go.clone());
            move |stem: &str| paused_exec(&r, &g, &tar_command(stem))
        };
        let mut s = setup(
            &f,
            if case < 2 {
                &paused_probe
            } else {
                &plain_probe
            },
        );
        s.tar = match case {
            0 => &tar_command,
            1 => &missing,
            2 | 3 => &paused_tar,
            _ => &paused_real_tar,
        };
        let got = package_with(&f.binary, V, TARGET, &f.out(), &s);
        placed
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{what}: the newcomer was placed"));
        assert_eq!(got, Err(Failure::FileOperation), "{what}");
        assert_eq!(
            fs::read(&archive).expect("the newcomer"),
            b"newcomer\n",
            "{what}: the newcomer is untouched"
        );
        assert_eq!(
            names(&f.out()),
            vec![format!("{STEM}.tar.gz")],
            "{what}: OUT holds only the newcomer"
        );
        let mut after = f.state();
        after[2].1.retain(|n| n != "out");
        assert_eq!(after, before, "{what}: the inputs are unchanged");
    }
}

/// A decoy OUT at `out`: a stage of its own, as staged inputs and as a staged release, with
/// an unprobed binary and other licenses.
fn make_decoy(out: &Path, stem: &str) {
    for d in [out.join(STAGE_DIR), out.join(STAGE_DIR).join(stem)] {
        fs::create_dir_all(&d).expect("mkdir");
        for (name, bytes) in [
            ("rdy", &b"an unprobed binary\n"[..]),
            (MIT_FILE, b"a decoy license\n"),
            (APACHE_FILE, b"a decoy license\n"),
        ] {
            fs::write(d.join(name), bytes).expect("write");
        }
    }
}

#[test]
fn pre1_package_keeps_to_its_own_out_when_out_is_replaced() {
    const STEM: &str = "rdy-0.14.0-preview-aarch64-apple-darwin";
    if !tar_or_skip(&crate::test_name!(
        "pre1_package_keeps_to_its_own_out_when_out_is_replaced"
    )) {
        return;
    }
    let reference = tempfile::tempdir().expect("tempdir");
    make_decoy(reference.path(), STEM);
    let decoy = tree_of(reference.path());
    // Someone who can write OUT's parent moves OUT aside and puts a decoy in its place: while
    // the probe runs, and after it, before the real tar starts.
    for (case, what) in [
        "OUT replaced while the probe runs",
        "OUT replaced before tar runs",
    ]
    .into_iter()
    .enumerate()
    {
        let f = Fixture::new();
        let before = f.state();
        let (ready, go) = (f.dir.path().join("ready"), f.dir.path().join("go"));
        fifo(&ready);
        fifo(&go);
        let moved = f.parent.join("moved");
        let replaced = on_ready(ready.clone(), go.clone(), {
            let (out, moved) = (f.out(), moved.clone());
            move || {
                fs::rename(&out, &moved).expect("move OUT aside");
                make_decoy(&out, STEM);
            }
        });
        let paused_probe = {
            let (r, g) = (ready.clone(), go.clone());
            move |_: &Path| paused(&r, &g, "printf 'rdy 0.14.0 (preview)\\n'")
        };
        let plain_probe = printing("rdy 0.14.0 (preview)\\n");
        let paused_real_tar = {
            let (r, g) = (ready.clone(), go.clone());
            move |stem: &str| paused_exec(&r, &g, &tar_command(stem))
        };
        let mut s = setup(
            &f,
            if case == 0 {
                &paused_probe
            } else {
                &plain_probe
            },
        );
        if case == 1 {
            s.tar = &paused_real_tar;
        }
        let got = package_with(&f.binary, V, TARGET, &f.out(), &s);
        replaced
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{what}: OUT was replaced"));
        unlock(&moved);
        assert_eq!(tree_of(&f.out()), decoy, "{what}: the decoy is untouched");
        assert_eq!(
            tree_of(&moved),
            Vec::new(),
            "{what}: nothing of this run is left in its own OUT"
        );
        assert_eq!(got, Err(Failure::FileOperation), "{what}");
        let mut after = f.state();
        after[2].1.retain(|n| n != "out" && n != "moved");
        assert_eq!(after, before, "{what}: the inputs are unchanged");
    }
}

/// Sets OUT and each directory in it back to 0700, so the fixture can be removed.
fn unlock(out: &Path) {
    if fs::symlink_metadata(out).is_err() {
        return;
    }
    crate::platform::set_mode(out, 0o700).expect("unlock OUT");
    for e in fs::read_dir(out).expect("read_dir") {
        let p = e.expect("entry").path();
        if fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
            crate::platform::set_mode(&p, 0o700).expect("unlock the stage");
        }
    }
}

#[test]
fn pre1_package_cleanup_failure_takes_precedence() {
    let name = crate::test_name!("pre1_package_cleanup_failure_takes_precedence");
    let probe_dir = tempfile::tempdir().expect("tempdir");
    crate::platform::set_mode(probe_dir.path(), 0o500).expect("chmod");
    let writable = fs::write(probe_dir.path().join("x"), b"x").is_ok();
    crate::platform::set_mode(probe_dir.path(), 0o700).expect("chmod");
    if writable {
        skip::skip(
            &name,
            Why::Prerequisite,
            "a read-only directory takes writes here (running as root?)",
        );
        return;
    }
    // Each probe or tar makes a directory that the cleanup must empty read-only.
    let locking = |script: &'static str| {
        move |bin: &Path| {
            let mut c = shell_script(script);
            c.arg(bin);
            c
        }
    };
    let rejected = locking("chmod 500 \"${0%/*}\"; printf 'rdy 0.13.0\\n'");
    let overflow = locking("chmod 500 \"${0%/*}\"; head -c 1048577 /dev/zero");
    let fine = printing("rdy 0.14.0\\n");
    // tar starts in OUT
    let locking_tar = |stem: &str| {
        let mut c = shell_script("tar -czf - \"$0\" && chmod 500 .");
        c.arg(stem).env("COPYFILE_DISABLE", "1");
        c
    };
    type Probe<'a> = &'a dyn Fn(&Path) -> Command;
    let cases: [(&str, Probe, Option<TarCommand>, u64); 3] = [
        ("a version rejection", &rejected, None, ARCHIVE_MAX),
        ("an output overflow", &overflow, None, ARCHIVE_MAX),
        ("an archive over its cap", &fine, Some(&locking_tar), 1),
    ];
    for (what, probe, tar, archive_max) in cases {
        if tar.is_some() && !tar_or_skip(&name) {
            continue;
        }
        let f = Fixture::new();
        let before = f.state();
        let mut s = setup(&f, probe);
        s.archive_max = archive_max;
        if let Some(tar) = tar {
            s.tar = tar;
        }
        let got = package_with(&f.binary, V, TARGET, &f.out(), &s);
        unlock(&f.out());
        assert_eq!(
            got,
            Err(Failure::FileOperation),
            "{what}: the cleanup failure is the result"
        );
        let mut after = f.state();
        after[2].1.retain(|n| n != "out");
        assert_eq!(after, before, "{what}: the inputs are unchanged");
    }
}
