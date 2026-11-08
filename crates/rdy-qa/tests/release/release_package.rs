//! `qa-release-package` and the dual license of the workspace.
use rdy_qa::fixtures::output_within;
use rdy_qa::platform;
use rdy_qa::release_install::{
    self, BINARY_MAX, MSG_FILE_OPERATION, MSG_INVALID_ARGUMENTS, MSG_INVALID_INPUT,
    MSG_OUTPUT_EXISTS, MSG_SIZE_LIMIT, TAR_SKIP_REASON, TARGETS, WINDOWS_TARGET,
};
use rdy_qa::skip::{self, Why};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

/// Every release target the tool knows: the two Unix ones and Windows.
fn all_targets() -> [&'static str; 3] {
    [TARGETS[0], TARGETS[1], WINDOWS_TARGET]
}

/// An absolute path of exactly `len` bytes whose last entry does not exist.
fn long_path(len: usize) -> String {
    let root = if cfg!(windows) { "C:\\" } else { "/" };
    format!("{root}{}", "a".repeat(len - root.len()))
}

/// This machine's release target, the only one the package tool packages for (its version
/// probe runs the binary here). On a machine with none, such as Linux arm64, the tool refuses
/// every target as invalid arguments, which this asserts for `binary` and `out`; then `None`.
/// The unit pins cover packaging there through a stand-in setup.
pub(crate) fn native_or_refused(binary: &OsStr, out: &Path) -> Option<&'static str> {
    if let Ok(target) = platform::release_target() {
        return Some(target);
    }
    for t in all_targets() {
        let what = format!("{t} on a machine with no release target");
        refused(
            &run(&flags(binary, "0.14.0", t, out.as_os_str())),
            2,
            MSG_INVALID_ARGUMENTS,
            &what,
        );
        assert!(fs::symlink_metadata(out).is_err(), "{what}: no OUT");
    }
    None
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Runs `qa-release-package` with `args`.
pub(crate) fn run<S: AsRef<OsStr>>(args: &[S]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qa-release-package"));
    cmd.args(args);
    output_within(&mut cmd, Duration::from_secs(600)).expect("run qa-release-package")
}

/// The four flags, each with its value.
pub(crate) fn flags(binary: &OsStr, version: &str, target: &str, out: &OsStr) -> Vec<OsString> {
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
        "rdy-diagram",
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
    assert_eq!(got, want, "all twelve crates are MIT OR Apache-2.0");

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
    let Some(target) = native_or_refused(b, &out) else {
        return;
    };

    // Arguments: exit 2, one fixed text, nothing created.
    let good = flags(b, "0.14.0", target, o);
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
    twice.extend(["--target".into(), target.into()]);
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
        cases.push(("a bad version", flags(b, v, target, o)));
    }
    for t in ["x86_64-apple-darwin", "aarch64-unknown-linux-gnu", ""] {
        cases.push(("a bad target", flags(b, "0.14.0", t, o)));
    }
    // The other release target: the version probe runs the binary here, so it is native.
    for t in all_targets().into_iter().filter(|t| *t != target) {
        cases.push(("another machine's target", flags(b, "0.14.0", t, o)));
    }
    let long = long_path(4097);
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
        cases.push((what, flags(&binary, "0.14.0", target, &out_arg)));
    }
    for (what, args) in &cases {
        refused(&run(args), 2, MSG_INVALID_ARGUMENTS, what);
        assert!(fs::symlink_metadata(&out).is_err(), "{what}: no OUT");
    }
    // A NUL cannot reach a command line; the library refuses it the same way.
    assert_eq!(
        release_install::package(Path::new("/a\0b"), "0.14.0", target, &out),
        Err(MSG_INVALID_ARGUMENTS.to_string())
    );
    assert_eq!(
        release_install::package(&bin, "0.14.0", target, Path::new("/a\0b")),
        Err(MSG_INVALID_ARGUMENTS.to_string())
    );

    // Input files: exit 1.
    let path_4096 = long_path(4096);
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
            &run(&flags(binary.as_os_str(), "0.14.0", target, o)),
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
    for (what, taken) in [
        ("a directory", &occupied),
        ("a file", &file_out),
        ("a dangling symlink", &dangling),
    ] {
        refused(
            &run(&flags(b, "0.14.0", target, taken.as_os_str())),
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
            target,
            occupied.as_os_str(),
        )),
        1,
        MSG_INVALID_INPUT,
        "a missing binary and an occupied OUT",
    );
    // OUT's parent must be a directory.
    for parent in [missing.join("out"), file_out.join("out")] {
        refused(
            &run(&flags(b, "0.14.0", target, parent.as_os_str())),
            1,
            MSG_FILE_OPERATION,
            "OUT's parent is not a directory",
        );
    }
    assert_eq!(fs::read(&bin).expect("binary"), b"rdy fixture\n");

    // A binary of exactly the cap is packaged: a sparse file that answers --version.
    if !tar_available_or_skip(&rdy_qa::test_name!(
        "pre1_package_refuses_inputs_and_never_overwrites"
    )) {
        return;
    }
    let full = dir.path().join("full");
    let (version, stem) = answering_binary(&full, target);
    let f = fs::OpenOptions::new()
        .write(true)
        .open(&full)
        .expect("open");
    f.set_len(BINARY_MAX).expect("a sparse file at the cap");
    drop(f);
    let done = run(&flags(full.as_os_str(), &version, target, o));
    let archive = out.join(format!("{stem}.tar.gz"));
    assert_eq!(done.status.code(), Some(0), "{done:?}");
    assert_eq!(
        String::from_utf8_lossy(&done.stdout),
        format!("{}\n", archive.display())
    );
    assert!(done.stderr.is_empty());
    assert!(fs::symlink_metadata(&archive).expect("archive").is_file());
    assert_eq!(fs::read_dir(&out).expect("read_dir").count(), 1);
}

/// The `--version` line of the built `rdy`, without its LF, and its plain version.
fn built_identity() -> (String, String) {
    let mut cmd = Command::new(rdy_qa::fixtures::rdy_bin());
    cmd.arg("--version");
    let out = output_within(&mut cmd, Duration::from_secs(600)).expect("run rdy --version");
    assert!(out.status.success() && out.stderr.is_empty(), "{out:?}");
    let line = String::from_utf8(out.stdout)
        .expect("utf-8")
        .strip_suffix('\n')
        .expect("one LF")
        .to_string();
    let version = line
        .strip_prefix("rdy ")
        .and_then(|v| v.split(' ').next())
        .expect("rdy <version>")
        .to_string();
    (line, version)
}

/// Writes at `path` a small executable that answers `--version` for the package tool, and
/// returns the version it answers and the archive stem it gets for `target`. On Unix it is a
/// script that prints `rdy 0.14.0`. Windows has no script that a probe can run as `rdy.exe`, so
/// it is a copy of the built `rdy`, which answers with its own version.
fn answering_binary(path: &Path, target: &str) -> (String, String) {
    if cfg!(windows) {
        fs::copy(rdy_qa::fixtures::rdy_bin(), path).expect("copy the built rdy");
        let (line, version) = built_identity();
        let stem = if line.ends_with(" (preview)") {
            format!("rdy-{version}-preview-{target}")
        } else {
            format!("rdy-{version}-{target}")
        };
        (version, stem)
    } else {
        fs::write(path, b"#!/bin/sh\nprintf 'rdy 0.14.0\\n'\nexit 0\n").expect("script");
        ("0.14.0".to_string(), format!("rdy-0.14.0-{target}"))
    }
}

pub(crate) fn tar_available_or_skip(test: &str) -> bool {
    if release_install::tar_available() {
        return true;
    }
    skip::skip(test, Why::Prerequisite, TAR_SKIP_REASON);
    false
}

/// The members of a `.tar.gz` archive, sorted, from the system's own `tar`.
fn members_of(archive: &Path) -> Vec<String> {
    let mut cmd = Command::new(platform::tar_program());
    cmd.arg("-tzf").arg(archive);
    let out = output_within(&mut cmd, Duration::from_secs(600)).expect("run tar -tzf");
    assert!(out.status.success(), "{out:?}");
    let mut v: Vec<String> = String::from_utf8(out.stdout)
        .expect("utf-8 listing")
        .lines()
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

#[test]
fn wx35a_native_package_run_takes_the_native_branch() {
    // a machine with a release target packages for it, and the tool does not refuse it as
    // another machine's target; a machine with none refuses every target (see native_or_refused)
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("out");
    let got = native_or_refused(dir.path().join("rdy").as_os_str(), &out);
    assert_eq!(got, platform::release_target().ok());
    if cfg!(windows) {
        assert_eq!(got, Some(WINDOWS_TARGET), "Windows is MSVC amd64");
    }
    let Some(target) = got else {
        return;
    };
    if !tar_available_or_skip(&rdy_qa::test_name!(
        "wx35a_native_package_run_takes_the_native_branch"
    )) {
        return;
    }
    // the built rdy is the binary: its archive holds it as rdy or rdy.exe, byte for byte, and
    // the packaged copy answers with the same build identity
    let built = rdy_qa::fixtures::rdy_bin();
    let (line, version) = built_identity();
    let stem = if line.ends_with(" (preview)") {
        format!("rdy-{version}-preview-{target}")
    } else {
        format!("rdy-{version}-{target}")
    };
    let exe = if target == WINDOWS_TARGET {
        "rdy.exe"
    } else {
        "rdy"
    };
    let done = run(&flags(built.as_os_str(), &version, target, out.as_os_str()));
    assert_eq!(done.status.code(), Some(0), "{done:?}");
    let archive = out.join(format!("{stem}.tar.gz"));
    assert_eq!(
        String::from_utf8_lossy(&done.stdout),
        format!("{}\n", archive.display())
    );
    let mut want = vec![
        format!("{stem}/"),
        format!("{stem}/LICENSE-APACHE"),
        format!("{stem}/LICENSE-MIT"),
        format!("{stem}/THIRD-PARTY-NOTICES.txt"),
        format!("{stem}/{exe}"),
    ];
    if line.ends_with(" (preview)") {
        want.push(format!("{stem}/PREVIEW.txt"));
    }
    want.sort();
    assert_eq!(members_of(&archive), want);
    let into = dir.path().join("x");
    fs::create_dir(&into).expect("mkdir");
    let mut cmd = Command::new(platform::tar_program());
    cmd.arg("-xzf").arg(&archive).arg("-C").arg(&into);
    let un = output_within(&mut cmd, Duration::from_secs(600)).expect("run tar -xzf");
    assert!(un.status.success(), "{un:?}");
    let packaged = into.join(&stem).join(exe);
    assert_eq!(
        fs::read(&packaged).expect("packaged"),
        fs::read(&built).expect("built"),
        "the archive holds the built executable"
    );
    let mut cmd = Command::new(&packaged);
    cmd.arg("--version");
    let ran = output_within(&mut cmd, Duration::from_secs(600)).expect("run the packaged rdy");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), format!("{line}\n"));
}
