//! `qa-release-package` and the dual license of the workspace.
use rdy_qa::fixtures::output_within;
use rdy_qa::platform;
use rdy_qa::release_install::{
    self, BINARY_MAX, MSG_FILE_OPERATION, MSG_INVALID_ARGUMENTS, MSG_INVALID_INPUT,
    MSG_OUTPUT_EXISTS, MSG_SIZE_LIMIT, TAR_SKIP_REASON,
};
use rdy_qa::skip::{self, Why};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

const TARGET: &str = "x86_64-unknown-linux-gnu";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Runs `qa-release-package` with `args`.
fn run<S: AsRef<OsStr>>(args: &[S]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qa-release-package"));
    cmd.args(args);
    output_within(&mut cmd, Duration::from_secs(600)).expect("run qa-release-package")
}

/// The four flags, each with its value.
fn flags(binary: &OsStr, version: &str, target: &str, out: &OsStr) -> Vec<OsString> {
    vec![
        "--binary".into(),
        binary.into(),
        "--version".into(),
        version.into(),
        "--target".into(),
        target.into(),
        "--out".into(),
        out.into(),
    ]
}

/// Asserts a refusal: exit `code`, stderr exactly `message` and LF, and no stdout.
fn refused(out: &Output, code: i32, message: &str, what: &str) {
    assert_eq!(out.status.code(), Some(code), "{what}: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("{message}\n"),
        "{what}"
    );
    assert!(out.stdout.is_empty(), "{what}: no stdout on failure");
}

#[test]
fn pre1_workspace_license_covers_every_crate() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["metadata", "--no-deps", "--locked", "--format-version", "1"])
        .current_dir(root());
    let out = output_within(&mut cmd, Duration::from_secs(600)).expect("cargo metadata");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("metadata JSON");
    let mut got: Vec<(String, Option<String>)> = meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|p| {
            (
                p["name"].as_str().expect("name").to_string(),
                p["license"].as_str().map(str::to_string),
            )
        })
        .collect();
    got.sort();
    let want: Vec<(String, Option<String>)> = [
        "rdy-adapter",
        "rdy-cli",
        "rdy-daemon",
        "rdy-engine",
        "rdy-graph",
        "rdy-mcp",
        "rdy-model",
        "rdy-qa",
        "rdy-render",
        "rdy-schema",
        "rdy-store",
    ]
    .iter()
    .map(|n| (n.to_string(), Some("MIT OR Apache-2.0".to_string())))
    .collect();
    assert_eq!(got, want, "all eleven crates are MIT OR Apache-2.0");

    for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
        let m = fs::symlink_metadata(root().join(name)).expect(name);
        assert!(m.file_type().is_file() && m.len() > 0, "{name}");
    }
    assert!(
        fs::symlink_metadata(root().join("LICENSE")).is_err(),
        "LICENSE became LICENSE-APACHE"
    );
    let mit = fs::read_to_string(root().join("LICENSE-MIT")).expect("LICENSE-MIT");
    assert!(
        mit.starts_with("MIT License\n\nCopyright (c) 2026 rdy contributors\n\n"),
        "{mit}"
    );
    assert!(mit.ends_with("THE\nSOFTWARE.\n") || mit.ends_with(" SOFTWARE.\n"));
    assert!(!mit.contains('\r') && !mit.ends_with("\n\n"));
    let apache = fs::read_to_string(root().join("LICENSE-APACHE")).expect("LICENSE-APACHE");
    assert!(
        apache.contains("Apache License\n                           Version 2.0, January 2004")
    );
    assert!(apache.contains("APPENDIX: How to apply the Apache License to your work."));
}

#[test]
fn pre1_package_refuses_inputs_and_never_overwrites() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("rdy");
    fs::write(&bin, b"rdy fixture\n").expect("binary");
    let out = dir.path().join("out");
    let (b, o) = (bin.as_os_str(), out.as_os_str());

    let help = run(&["--help"]);
    assert_eq!(help.status.code(), Some(0), "{help:?}");
    let usage = String::from_utf8_lossy(&help.stdout);
    assert!(usage.contains("Usage: qa-release-package"), "{usage}");
    assert!(help.stderr.is_empty());

    // Arguments: exit 2, one fixed text, nothing created.
    let good = flags(b, "0.14.0", TARGET, o);
    let mut cases: Vec<(&str, Vec<OsString>)> = vec![("no arguments", vec![])];
    for (i, what) in ["no --binary", "no --version", "no --target", "no --out"]
        .into_iter()
        .enumerate()
    {
        let mut v = good.clone();
        v.drain(2 * i..2 * i + 2);
        cases.push((what, v));
    }
    let mut twice = good.clone();
    twice.extend(["--target".into(), TARGET.into()]);
    cases.push(("--target twice", twice));
    let mut unknown = good.clone();
    unknown.extend(["--sign".into(), "x".into()]);
    cases.push(("an unknown flag", unknown));
    let mut positional = good.clone();
    positional.push("extra".into());
    cases.push(("a positional argument", positional));
    for v in [
        "0.14",
        "1.0.0",
        "0.0.0",
        "0.14.0-preview",
        "0.12345678901.0",
    ] {
        cases.push(("a bad version", flags(b, v, TARGET, o)));
    }
    for t in ["x86_64-apple-darwin", "aarch64-unknown-linux-gnu", ""] {
        cases.push(("a bad target", flags(b, "0.14.0", t, o)));
    }
    let long = format!("/{}", "a".repeat(4096));
    for (what, binary, out_arg) in [
        ("a relative binary", OsString::from("rdy"), o.to_owned()),
        ("a relative OUT", b.to_owned(), OsString::from("out")),
        ("an empty binary path", OsString::new(), o.to_owned()),
        (
            "a 4097-byte binary path",
            OsString::from(&long),
            o.to_owned(),
        ),
        ("a 4097-byte OUT", b.to_owned(), OsString::from(&long)),
        (
            "an LF in OUT",
            b.to_owned(),
            OsString::from(format!("{}\n", out.display())),
        ),
        (
            "a CR in the binary path",
            OsString::from(format!("{}\r", bin.display())),
            o.to_owned(),
        ),
    ] {
        cases.push((what, flags(&binary, "0.14.0", TARGET, &out_arg)));
    }
    for (what, args) in &cases {
        refused(&run(args), 2, MSG_INVALID_ARGUMENTS, what);
        assert!(fs::symlink_metadata(&out).is_err(), "{what}: no OUT");
    }
    // A NUL cannot reach a command line; the library refuses it the same way.
    assert_eq!(
        release_install::package(Path::new("/a\0b"), "0.14.0", TARGET, &out),
        Err(MSG_INVALID_ARGUMENTS.to_string())
    );
    assert_eq!(
        release_install::package(&bin, "0.14.0", TARGET, Path::new("/a\0b")),
        Err(MSG_INVALID_ARGUMENTS.to_string())
    );

    // Input files: exit 1.
    let path_4096 = format!("/{}", "a".repeat(4095));
    let missing = dir.path().join("missing");
    let empty = dir.path().join("empty");
    fs::write(&empty, b"").expect("empty");
    let link = dir.path().join("link");
    platform::symlink(&bin, &link).expect("symlink");
    let over = dir.path().join("over");
    let f = fs::File::create(&over).expect("create");
    f.set_len(BINARY_MAX + 1)
        .expect("a sparse file one byte over the cap");
    drop(f);
    for (what, binary, want) in [
        (
            "a 4096-byte path to nothing",
            PathBuf::from(&path_4096),
            MSG_INVALID_INPUT,
        ),
        ("a missing binary", missing.clone(), MSG_INVALID_INPUT),
        ("an empty binary", empty.clone(), MSG_INVALID_INPUT),
        ("a directory", dir.path().to_path_buf(), MSG_INVALID_INPUT),
        ("a symlink", link.clone(), MSG_INVALID_INPUT),
        ("one byte over the cap", over.clone(), MSG_SIZE_LIMIT),
    ] {
        refused(
            &run(&flags(binary.as_os_str(), "0.14.0", TARGET, o)),
            1,
            want,
            what,
        );
        assert!(fs::symlink_metadata(&out).is_err(), "{what}: no OUT");
    }

    // An occupied OUT is never touched.
    let occupied = dir.path().join("occupied");
    fs::create_dir(&occupied).expect("mkdir");
    fs::write(occupied.join("keep"), b"keep\n").expect("sentinel");
    let file_out = dir.path().join("file-out");
    fs::write(&file_out, b"mine\n").expect("file");
    let dangling = dir.path().join("dangling");
    platform::symlink(&dir.path().join("nowhere"), &dangling).expect("symlink");
    for (what, target) in [
        ("a directory", &occupied),
        ("a file", &file_out),
        ("a dangling symlink", &dangling),
    ] {
        refused(
            &run(&flags(b, "0.14.0", TARGET, target.as_os_str())),
            1,
            MSG_OUTPUT_EXISTS,
            what,
        );
    }
    assert_eq!(
        fs::read(occupied.join("keep")).expect("sentinel"),
        b"keep\n"
    );
    assert_eq!(fs::read_dir(&occupied).expect("read_dir").count(), 1);
    assert_eq!(fs::read(&file_out).expect("file"), b"mine\n");
    assert_eq!(
        fs::read_link(&dangling).expect("link"),
        dir.path().join("nowhere")
    );
    assert!(fs::symlink_metadata(dir.path().join("nowhere")).is_err());
    // The binary is checked before OUT.
    refused(
        &run(&flags(
            missing.as_os_str(),
            "0.14.0",
            TARGET,
            occupied.as_os_str(),
        )),
        1,
        MSG_INVALID_INPUT,
        "a missing binary and an occupied OUT",
    );
    // OUT's parent must be a directory.
    for parent in [missing.join("out"), file_out.join("out")] {
        refused(
            &run(&flags(b, "0.14.0", TARGET, parent.as_os_str())),
            1,
            MSG_FILE_OPERATION,
            "OUT's parent is not a directory",
        );
    }
    assert_eq!(fs::read(&bin).expect("binary"), b"rdy fixture\n");

    // A binary of exactly the cap is packaged: a sparse file whose script answers --version.
    if !tar_available_or_skip(&rdy_qa::test_name!(
        "pre1_package_refuses_inputs_and_never_overwrites"
    )) {
        return;
    }
    let full = dir.path().join("full");
    fs::write(&full, b"#!/bin/sh\nprintf 'rdy 0.14.0\\n'\nexit 0\n").expect("script");
    let f = fs::OpenOptions::new()
        .write(true)
        .open(&full)
        .expect("open");
    f.set_len(BINARY_MAX).expect("a sparse file at the cap");
    drop(f);
    let done = run(&flags(full.as_os_str(), "0.14.0", TARGET, o));
    let archive = out.join(format!("rdy-0.14.0-{TARGET}.tar.gz"));
    assert_eq!(done.status.code(), Some(0), "{done:?}");
    assert_eq!(
        String::from_utf8_lossy(&done.stdout),
        format!("{}\n", archive.display())
    );
    assert!(done.stderr.is_empty());
    assert!(fs::symlink_metadata(&archive).expect("archive").is_file());
    assert_eq!(fs::read_dir(&out).expect("read_dir").count(), 1);
}

fn tar_available_or_skip(test: &str) -> bool {
    if release_install::tar_available() {
        return true;
    }
    skip::skip(test, Why::Prerequisite, TAR_SKIP_REASON);
    false
}

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
    cmd.args(flags(bin.as_os_str(), "0.14.0", TARGET, out.as_os_str()));
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
    let archive = out.join(format!("rdy-0.14.0-{TARGET}.tar.gz"));
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
