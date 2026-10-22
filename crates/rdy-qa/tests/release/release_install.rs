#![cfg(unix)]
// Unix only: a token that is not UTF-8 is made with OsStrExt for the guide renderer's refusal.
//! `qa-release-install` with the actual release build, and the preview note's three surfaces.
use rdy_qa::fixtures::{output_within, rdy_bin};
use rdy_qa::platform::{self, RELEASE_CHILD_DEADLINE, RELEASE_CHILD_OUTPUT_MAX};
use rdy_qa::release_install::{
    self, GuideCommand, InstallInput, MSG_INSTALL_INVALID_ARGUMENTS, MSG_INVALID_ARGUMENTS,
    MSG_INVALID_COMMAND, PREVIEW_NOTE, TAR_SKIP_REASON, TARGETS,
};
use rdy_qa::skip::{self, Why};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// The hang guard of this file's own tool runs.
const GUARD: Duration = Duration::from_secs(600);
/// A commit S for the receipt: the checker checks its syntax only.
const COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Runs `program` with `args`, which must succeed; its stdout.
fn tool<S: AsRef<OsStr>>(program: &str, args: &[S]) -> Vec<u8> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    let out = platform::run_bounded(cmd, None, RELEASE_CHILD_OUTPUT_MAX, GUARD)
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    assert!(
        out.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Runs `qa-release-install` with `args`.
fn install<S: AsRef<OsStr>>(args: &[S]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qa-release-install"));
    cmd.args(args);
    output_within(&mut cmd, GUARD).expect("run qa-release-install")
}

/// The CLI arguments for `input`.
fn flags(input: &InstallInput) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec![
        "--download".into(),
        input.download.clone().into(),
        "--signer-line".into(),
        input.signer_line.clone().into(),
        "--version".into(),
        input.version.clone().into(),
        "--commit".into(),
        input.commit.clone().into(),
        "--target".into(),
        input.target.clone().into(),
        "--unpack".into(),
        input.unpack.clone().into(),
        "--prefix".into(),
        input.prefix.clone().into(),
    ];
    if input.browser {
        v.push("--browser".into());
    }
    v
}

/// The plain version of the built CLI.
const V: &str = env!("CARGO_PKG_VERSION");

/// Every entry under `dir` relative to it, sorted.
fn entries(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, into: &mut Vec<String>) {
        for e in fs::read_dir(dir).expect("read_dir") {
            let p = e.expect("entry").path();
            into.push(
                p.strip_prefix(root)
                    .expect("under the root")
                    .to_string_lossy()
                    .into_owned(),
            );
            if fs::symlink_metadata(&p).expect("stat").is_dir() {
                walk(root, &p, into);
            }
        }
    }
    let mut v = Vec::new();
    walk(dir, dir, &mut v);
    v.sort();
    v
}

/// Whether any running process's command line names `binary`.
fn runs(binary: &Path) -> bool {
    let out = tool("ps", &["-A", "-ww", "-o", "args="]);
    String::from_utf8_lossy(&out)
        .lines()
        .any(|l| l.contains(&*binary.to_string_lossy()))
}

#[test]
#[ignore = "requires a native preview archive target"]
fn pre3_installed_binary_runs_cli_and_mcp_outside_the_build_tree() {
    // Native release qualification: no skip path. A missing tool or target fails the test.
    let target = platform::release_target().expect("a native preview archive target");
    assert!(
        release_install::install_tools_available(target),
        "{}",
        release_install::INSTALL_SKIP_REASON
    );
    let rdy = rdy_bin();
    let work = tempfile::tempdir().expect("tempdir");

    // The actual release binary, packaged by the release tool, in a private download.
    let archive =
        release_install::package(&rdy, V, target, &work.path().join("out")).expect("package");
    let download = work.path().join("download");
    fs::create_dir(&download).expect("mkdir");
    platform::set_mode(&download, 0o700).expect("private download");
    let name = release_install::archive_name(V, target);
    fs::copy(&archive, download.join(&name)).expect("copy the archive");

    // Two manifest lines (only the native archive is present), signed by a disposable test key.
    // This key is never a release key.
    let lines: String = TARGETS
        .iter()
        .map(|t| {
            let file = download.join(release_install::archive_name(V, t));
            let hash = if file.exists() {
                let out = if *t == TARGETS[0] {
                    tool(
                        "shasum",
                        &[OsStr::new("-a"), "256".as_ref(), file.as_os_str()],
                    )
                } else {
                    tool("sha256sum", &[file.as_os_str()])
                };
                String::from_utf8(out).expect("utf-8")[..64].to_string()
            } else {
                "ab".repeat(32)
            };
            format!("{hash}  {}\n", release_install::archive_name(V, t))
        })
        .collect();
    let manifest = download.join(release_install::MANIFEST_FILE);
    fs::write(&manifest, &lines).expect("manifest");
    let key = work.path().join("test-key");
    tool(
        "ssh-keygen",
        &[
            OsStr::new("-q"),
            "-t".as_ref(),
            "ed25519".as_ref(),
            "-N".as_ref(),
            "".as_ref(),
            "-C".as_ref(),
            "".as_ref(),
            "-f".as_ref(),
            key.as_os_str(),
        ],
    );
    tool(
        "ssh-keygen",
        &[
            OsStr::new("-Y"),
            "sign".as_ref(),
            "-f".as_ref(),
            key.as_os_str(),
            "-n".as_ref(),
            release_install::RELEASE_NAMESPACE.as_ref(),
            "-O".as_ref(),
            "hashalg=sha512".as_ref(),
            manifest.as_os_str(),
        ],
    );
    let public = fs::read_to_string(format!("{}.pub", key.display())).expect("public key");
    let words: Vec<&str> = public.split_whitespace().take(2).collect();
    let signer = format!(
        "rdy-release namespaces=\"rdy-release-v1\" {} {}",
        words[0], words[1]
    );
    let downloaded: Vec<(String, Vec<u8>)> = entries(&download)
        .into_iter()
        .map(|n| {
            let bytes = fs::read(download.join(&n)).expect("read");
            (n, bytes)
        })
        .collect();

    // Installed into a version prefix under a spaced directory, outside the build tree.
    let spaced = work.path().join("a spaced dir");
    fs::create_dir(&spaced).expect("mkdir");
    let input = InstallInput {
        download: download.clone(),
        signer_line: signer,
        version: V.into(),
        commit: COMMIT.into(),
        target: target.into(),
        unpack: work.path().join("un pack"),
        prefix: spaced.join(format!("rdy-{V}")),
        browser: false,
    };
    let out = install(&flags(&input));
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let binary = input.prefix.join("bin").join("rdy");
    let os = platform::release_os().expect("os");
    let checks: Vec<String> = [
        "signature",
        "checksums",
        "install",
        "version",
        "daemon",
        "mcp",
    ]
    .iter()
    .map(|n| format!("{{\"name\":\"{n}\",\"passed\":true}}"))
    .collect();
    assert_eq!(
        String::from_utf8(out.stdout).expect("utf-8"),
        format!(
            "{{\"binary\":{},\"version\":\"{V}\",\"commit\":\"{COMMIT}\",\"target\":\"{target}\",\"os\":{},\"checks\":[{}],\"skipped\":0}}\n",
            serde_json::to_string(&binary).expect("json"),
            serde_json::to_string(&os).expect("json"),
            checks.join(",")
        )
    );

    // The installed product is bin/rdy alone, the verified archive's bytes, marked as a preview.
    assert_eq!(entries(&input.prefix), ["bin", "bin/rdy"]);
    let unpacked = work.path().join("check");
    fs::create_dir(&unpacked).expect("mkdir");
    tool(
        "tar",
        &[
            OsStr::new("-xzf"),
            download.join(&name).as_os_str(),
            "-C".as_ref(),
            unpacked.as_os_str(),
        ],
    );
    let stem = name.strip_suffix(".tar.gz").expect("stem");
    let verified = fs::read(unpacked.join(stem).join("rdy")).expect("archive binary");
    assert!(
        fs::read(&binary).expect("installed") == verified,
        "the installed bytes are the archive's"
    );
    assert!(
        verified == fs::read(&rdy).expect("built"),
        "the archive holds the built binary"
    );
    let mut version = Command::new(&binary);
    version.arg("--version");
    let version = platform::run_bounded(
        version,
        None,
        RELEASE_CHILD_OUTPUT_MAX,
        RELEASE_CHILD_DEADLINE,
    )
    .expect("--version");
    assert_eq!(version.stdout, format!("rdy {V} (preview)\n").into_bytes());

    // UNPACK and the signer file are gone, the download is unchanged, and no smoke daemon runs
    // (a hang guard while a stopped daemon leaves the process table, not a speed check).
    assert!(fs::symlink_metadata(&input.unpack).is_err());
    let after: Vec<(String, Vec<u8>)> = entries(&download)
        .into_iter()
        .map(|n| {
            let bytes = fs::read(download.join(&n)).expect("read");
            (n, bytes)
        })
        .collect();
    assert_eq!(after, downloaded);
    let until = Instant::now() + Duration::from_secs(10);
    while runs(&binary) {
        assert!(
            Instant::now() < until,
            "a process still runs {}",
            binary.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn pre3_preview_note_matches_readme_and_archive() {
    let readme = fs::read_to_string(root().join("README.md")).expect("README");
    let section = readme
        .split("<summary><b>About this preview</b></summary>\n")
        .nth(1)
        .expect("the About this preview block");
    let section = section.split("</details>").next().expect("block");
    assert!(
        section.contains(PREVIEW_NOTE),
        "the README's block holds the note's exact bytes"
    );
    assert!(
        PREVIEW_NOTE
            .ends_with("Report problems at https://github.com/adrenickchrome/rdy-core/issues.\n")
    );

    if !release_install::tar_available() {
        skip::skip(
            &rdy_qa::test_name!("pre3_preview_note_matches_readme_and_archive"),
            Why::Prerequisite,
            TAR_SKIP_REASON,
        );
        return;
    }
    let rdy = rdy_bin();
    let work = tempfile::tempdir().expect("tempdir");
    // The real release packages only for this machine's target. A machine with none (Linux
    // arm64) refuses both; there the unit pin `pre1_package_reads_the_binary_version` packages
    // the built CLI through a stand-in setup and checks its PREVIEW.txt against the note.
    let Ok(target) = platform::release_target() else {
        for t in TARGETS {
            assert_eq!(
                release_install::package(&rdy, V, t, &work.path().join("out")),
                Err(MSG_INVALID_ARGUMENTS.to_string()),
                "{t} on a machine with no release target"
            );
        }
        return;
    };
    let archive =
        release_install::package(&rdy, V, target, &work.path().join("out")).expect("package");
    let into = work.path().join("x");
    fs::create_dir(&into).expect("mkdir");
    tool(
        "tar",
        &[
            OsStr::new("-xzf"),
            archive.as_os_str(),
            "-C".as_ref(),
            into.as_os_str(),
        ],
    );
    let note = fs::read(
        into.join(format!("rdy-{V}-preview-{target}"))
            .join("PREVIEW.txt"),
    )
    .expect("PREVIEW.txt");
    assert!(note == PREVIEW_NOTE.as_bytes(), "PREVIEW.txt is the note");
}

#[test]
fn pre3_cli_prints_the_guide_and_refuses_bad_arguments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let key = ssh_key_line();
    let input = InstallInput {
        download: dir.path().join("no download"),
        signer_line: key,
        version: "0.14.0".into(),
        commit: COMMIT.into(),
        target: TARGETS[1].into(),
        unpack: dir.path().join("no unpack"),
        prefix: dir.path().join("no parent").join("rdy-0.14.0"),
        browser: false,
    };

    // Print mode: the rendered block and LF, for paths that do not exist, and nothing made.
    let mut args = flags(&input);
    args.push("--print-commands".into());
    let out = install(&args);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let block =
        release_install::render_commands(&release_install::commands(&input).expect("commands"))
            .expect("render");
    assert_eq!(
        String::from_utf8(out.stdout).expect("utf-8"),
        format!("{block}\n")
    );
    assert!(out.stderr.is_empty());
    assert_eq!(
        entries(dir.path()),
        Vec::<String>::new(),
        "print mode makes nothing"
    );

    // --help exits 0 with usage.
    let out = install(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let usage = String::from_utf8_lossy(&out.stdout);
    assert!(usage.contains("Usage"));
    // DOWNLOAD must be private: the help says so, as a plain mkdir under umask 0002 is not.
    assert!(
        usage.contains("owned by you and not writable by group or others")
            && usage.contains("mkdir -m 700"),
        "{usage}"
    );

    // Argument errors exit 2 with the one text, before any file work.
    let refused = |args: Vec<OsString>, what: &str| {
        let out = install(&args);
        assert_eq!(out.status.code(), Some(2), "{what}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("{MSG_INSTALL_INVALID_ARGUMENTS}\n"),
            "{what}"
        );
        assert!(out.stdout.is_empty(), "{what}");
    };
    let mut twice = flags(&input);
    twice.extend(["--version".into(), "0.14.0".into()]);
    refused(twice, "an option twice");
    let mut browser_twice = flags(&InstallInput {
        target: TARGETS[0].into(),
        browser: true,
        ..input.clone()
    });
    browser_twice.push("--browser".into());
    refused(browser_twice, "--browser twice");
    let mut positional = flags(&input);
    positional.push("extra".into());
    refused(positional, "a positional argument");
    refused(flags(&input)[2..].to_vec(), "a missing option");
    for (what, edit) in [
        (
            "a bad version",
            (|i: &mut InstallInput| i.version = "0.14".into()) as fn(&mut InstallInput),
        ),
        ("a bad commit", |i| i.commit = "abc".into()),
        ("a bad signer line", |i| i.signer_line.push_str(" comment")),
        ("a relative path", |i| i.unpack = "unpack".into()),
        ("the browser path on Linux", |i| i.browser = true),
    ] {
        let mut bad = input.clone();
        edit(&mut bad);
        refused(flags(&bad), what);
    }

    // A check failure exits 1 with its stage, and no report.
    let out = install(&flags(&input));
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "release install failed: input\n"
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn pre3_render_refuses_non_utf8_tokens() {
    use std::os::unix::ffi::OsStrExt;
    let at = PathBuf::from("/tmp");
    let command = |program: &OsStr| GuideCommand {
        program: program.to_os_string(),
        args: vec![],
        cwd: at.clone(),
        stdin: None,
        stdout: None,
    };
    let bad = OsStr::from_bytes(b"a\xffb");
    assert_eq!(
        release_install::render_commands(&[command(bad)]),
        Err(MSG_INVALID_COMMAND.to_string())
    );
    let mut redirected = command(OsStr::new("printf"));
    redirected.stdout = Some(PathBuf::from(bad));
    assert_eq!(
        release_install::render_commands(&[redirected]),
        Err(MSG_INVALID_COMMAND.to_string())
    );
    assert_eq!(
        release_install::render_commands(&[command(OsStr::new("true"))]),
        Ok("cd '/tmp' &&\n'true'".to_string())
    );
}

/// A signer line from fixed key bytes.
fn ssh_key_line() -> String {
    let key = ssh_key::PublicKey::new(
        ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey([9u8; 32])),
        "",
    );
    format!(
        "rdy-release namespaces=\"rdy-release-v1\" {}",
        key.to_openssh().expect("openssh")
    )
}
