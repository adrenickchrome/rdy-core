// Unix only: the install checker's stand-ins are shell scripts and the kit sets permission modes.
// The portable install tests (Task 36a) use the real tools and the built rdy, in the parent module
// and in tests/release/release_install.rs.
//! The Unix-only install checker fixtures of `release_install`.

use super::*;

/// A version the historical key signed: the README tests use it.
const RV: &str = "0.20.0";

const MCP_INIT: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"rdy","version":"0.14.0"}}}"#;
const MCP_SESSION: &str = r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"actor\":\"agent:release-check\"}"}]}}"#;
const MCP_OVERVIEW: &str =
    r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\"docs\":[]}"}]}}"#;

/// The SHA-256 of `file` in lowercase hex, from the host's checksum tool.
fn sha256(file: &Path) -> String {
    let out = if host_target() == MAC_TARGET {
        tool("shasum", &["-a".as_ref(), "256".as_ref(), file.as_os_str()])
    } else {
        tool("sha256sum", &[file.as_os_str()])
    };
    String::from_utf8(out).expect("utf-8")[..64].to_string()
}

/// A fake `rdy` (a shell script): it logs each call's arguments to `marker`, prints
/// `version` for `--version`, answers the smoke commands and the MCP session, and stops its
/// "daemon" with exit `stop`.
fn fake_rdy(marker: &Path, version: &str, stop: i32) -> String {
    fake_rdy_with(marker, version, stop, "")
}

/// [`fake_rdy`] whose MCP server runs `mcp_end` once its input closes.
fn fake_rdy_with(marker: &Path, version: &str, stop: i32, mcp_end: &str) -> String {
    format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{marker}'
case "$1" in
--version) printf '%s\n' '{version}' ;;
init) mkdir "$2" ;;
mcp)
  read -r l
  printf '%s\n' '{init}'
  read -r l
  read -r l
  printf '%s\n' '{session}'
  read -r l
  printf '%s\n' '{overview}'
  cat > /dev/null
  {mcp_end} ;;
--project)
  case "$3" in
  --json) printf '%s\n' '{{"views":[]}}' ;;
  serve) exit {stop} ;;
  esac ;;
esac
"#,
        marker = marker.display(),
        init = MCP_INIT,
        session = MCP_SESSION,
        overview = MCP_OVERVIEW,
    )
}

/// A download kit: a private DOWNLOAD holding a test archive for `target` with a fake
/// `rdy`, its four-line manifest and the manifest's signature by a disposable key.
struct Kit {
    dir: tempfile::TempDir,
    download: PathBuf,
    key: PathBuf,
    signer: String,
    target: &'static str,
    marker: PathBuf,
    scratch: PathBuf,
}

impl Kit {
    fn new(target: &'static str) -> Kit {
        Kit::with_rdy(target, "rdy 0.14.0", 0)
    }

    fn with_rdy(target: &'static str, version: &str, stop: i32) -> Kit {
        Kit::with_fake(target, &|marker| fake_rdy(marker, version, stop))
    }

    /// A kit whose archive's `rdy` is `fake(marker)`.
    fn with_fake(target: &'static str, fake: &dyn Fn(&Path) -> String) -> Kit {
        let dir = tempfile::tempdir().expect("tempdir");
        let download = dir.path().join("down load");
        let scratch = dir.path().join("scratch");
        for d in [&download, &scratch, &dir.path().join("pre fix")] {
            fs::create_dir(d).expect("mkdir");
        }
        platform::set_mode(&download, 0o700).expect("private download");
        fs::write(download.join("keep"), b"sentinel\n").expect("sentinel");
        let key = dir.path().join("key");
        let signer = test_key(&key);
        let marker = dir.path().join("ran");
        let kit = Kit {
            download,
            key,
            signer,
            target,
            marker,
            scratch,
            dir,
        };
        kit.build_archive(target, &fake(&kit.marker));
        kit.write_manifest(kit.lines().concat().as_bytes());
        kit
    }

    fn archive(&self, target: &str) -> PathBuf {
        self.download.join(archive_name(IV, target))
    }

    /// Writes the archive for `target` holding `rdy` and the other three files of a plain package.
    fn build_archive(&self, target: &str, rdy: &str) {
        let stem = archive_stem(IV, target);
        let src = self.dir.path().join(format!("src-{target}"));
        let root = src.join(&stem);
        fs::create_dir_all(&root).expect("mkdir");
        fs::write(root.join("rdy"), rdy).expect("rdy");
        platform::set_mode(&root.join("rdy"), 0o755).expect("chmod");
        fs::write(root.join(MIT_FILE), MIT).expect("mit");
        fs::write(root.join(APACHE_FILE), APACHE).expect("apache");
        self.retar(target);
    }

    /// The archive's source directory for `target` (`<stem>` holds its files).
    fn source(&self, target: &str) -> PathBuf {
        self.dir
            .path()
            .join(format!("src-{target}"))
            .join(archive_stem(IV, target))
    }

    /// Writes the archive for `target` again from its source directory, and the manifest
    /// for it.
    fn retar(&self, target: &str) {
        let src = self.dir.path().join(format!("src-{target}"));
        let _ = fs::remove_file(self.archive(target));
        let mut cmd = Command::new(platform::tar_program());
        cmd.arg("-czf")
            .arg(self.archive(target))
            .arg("-C")
            .arg(&src)
            .arg(archive_stem(IV, target))
            .env("COPYFILE_DISABLE", "1");
        let out = run_bounded(cmd, None, 1 << 20, GUARD).expect("tar");
        assert!(out.status.success(), "tar");
        if self.download.join(MANIFEST_FILE).exists() {
            self.write_manifest(self.lines().concat().as_bytes());
        }
    }

    /// The four manifest lines, in manifest order (macOS, Windows, Linux, sample): each
    /// present file's hash, else a stand-in hash.
    fn lines(&self) -> Vec<String> {
        let line = |name: String| {
            let file = self.download.join(&name);
            let hash = if file.exists() {
                sha256(&file)
            } else {
                "ab".repeat(32)
            };
            format!("{hash}  {name}\n")
        };
        MANIFEST_TARGETS
            .iter()
            .map(|t| line(archive_name(IV, t)))
            .chain([line(SAMPLE_FILE.to_string())])
            .collect()
    }

    /// The index of `target`'s line in [`Kit::lines`].
    fn at(target: &str) -> usize {
        MANIFEST_TARGETS
            .iter()
            .position(|t| *t == target)
            .expect("a manifest target")
    }

    /// Writes the sample dump `bytes` into DOWNLOAD and re-signs the manifest over it.
    fn add_sample(&self, bytes: &[u8]) {
        fs::write(self.download.join(SAMPLE_FILE), bytes).expect("sample");
        self.write_manifest(self.lines().concat().as_bytes());
    }

    /// Writes `bytes` as the manifest and signs it with the kit's key.
    fn write_manifest(&self, bytes: &[u8]) {
        let manifest = self.download.join(MANIFEST_FILE);
        fs::write(&manifest, bytes).expect("manifest");
        sign(&self.key, &manifest, RELEASE_NAMESPACE);
    }

    fn input(&self) -> InstallInput {
        InstallInput {
            download: self.download.clone(),
            signer_line: self.signer.clone(),
            version: IV.into(),
            commit: COMMIT.into(),
            target: self.target.into(),
            unpack: self.dir.path().join("un pack"),
            prefix: self.dir.path().join("pre fix").join("rdy-0.14.0"),
            browser: false,
        }
    }

    /// Every file in DOWNLOAD with its bytes, sorted.
    fn download_state(&self) -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<(String, Vec<u8>)> = names(&self.download)
            .into_iter()
            .map(|n| {
                let bytes = fs::read(self.download.join(&n)).unwrap_or_default();
                (n, bytes)
            })
            .collect();
        v.sort();
        v
    }

    /// The fake rdy's call log.
    fn ran(&self) -> Vec<String> {
        fs::read_to_string(&self.marker)
            .map(|t| t.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Asserts that nothing was extracted, installed or run, and that the signer file is
    /// gone again.
    fn assert_untouched(&self, input: &InstallInput, what: &str) {
        assert!(absent(&input.unpack), "{what}: no UNPACK");
        assert!(absent(&input.prefix), "{what}: no PREFIX");
        assert!(!self.marker.exists(), "{what}: the binary never ran");
        assert!(
            absent(&self.download.join(SIGNERS_FILE)),
            "{what}: the signer file is removed"
        );
        assert!(
            names(&self.scratch).is_empty(),
            "{what}: no scratch project"
        );
    }
}

/// The test's stand-in for each guide command: the real tool, except the checksum command
/// of the other target (the host's native tool instead) and `xattr`, which reports
/// `attributes` (a printf text) and removes nothing.
fn stand_in(c: &GuideCommand, attributes: &str) -> Command {
    if c.program == "shasum" && host_target() != MAC_TARGET {
        let mut cmd = Command::new("sha256sum");
        cmd.args(["-c", "--ignore-missing", MANIFEST_FILE]);
        return cmd;
    }
    if c.program == "sha256sum" && host_target() == MAC_TARGET {
        let mut cmd = Command::new("shasum");
        cmd.args(["-a", "256", "-c", "--ignore-missing", MANIFEST_FILE]);
        return cmd;
    }
    if c.program == "xattr" {
        return if c.args.first().is_some_and(|a| a == "-d") {
            shell_script("exit 0")
        } else {
            shell_script(&format!("printf '{attributes}'"))
        };
    }
    guide_process(c)
}

/// A guide command's argv as text.
fn argv(c: &GuideCommand) -> Vec<String> {
    std::iter::once(&c.program)
        .chain(&c.args)
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// Runs the check of `input` against `kit`, as a machine of the kit's target. `process`
/// stands in for the tools; `edit` changes the setup.
fn check_kit<'a>(
    kit: &Kit,
    input: &InstallInput,
    process: &'a dyn Fn(&GuideCommand) -> Command,
    edit: impl FnOnce(&mut InstallSetup<'a>),
) -> Result<InstalledRelease, InstallFailure> {
    let native: &'static dyn Fn() -> io::Result<&'static str> = if kit.target == MAC_TARGET {
        &as_mac
    } else {
        &as_linux
    };
    let mut setup = InstallSetup {
        native_target: native,
        os: &platform::release_os,
        process,
        ran: &nothing_ran,
        scratch: kit.scratch.clone(),
        max_output: RELEASE_CHILD_OUTPUT_MAX,
        deadline: RELEASE_CHILD_DEADLINE,
    };
    edit(&mut setup);
    check_with(input, &setup)
}

/// A macOS arm64 machine.
fn as_mac() -> io::Result<&'static str> {
    Ok(TARGETS[0])
}

/// A Linux amd64 machine.
fn as_linux() -> io::Result<&'static str> {
    Ok(TARGETS[1])
}

/// [`check_kit`] with the plain stand-ins and the setup unchanged.
fn check_plain(kit: &Kit, input: &InstallInput) -> Result<InstalledRelease, InstallFailure> {
    check_kit(kit, input, &|c| stand_in(c, ""), |_| {})
}

/// Runs the check of `input` against `kit` with the plain stand-ins (`attributes` for
/// `xattr`): its result and every guide command its runner ran, as the runner's hook saw
/// it. From the first guide command to the last, the runner starts no process but these
/// commands: the count of every process it starts goes up by exactly one per command.
fn check_recorded(
    kit: &Kit,
    input: &InstallInput,
    attributes: &str,
) -> (Result<InstalledRelease, InstallFailure>, Vec<Shown>) {
    let ran = Mutex::new(Vec::new());
    let record = |c: &GuideCommand| {
        let started = platform::runner_starts();
        ran.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((shown(c), started));
    };
    let process = |c: &GuideCommand| stand_in(c, attributes);
    let got = check_kit(kit, input, &process, |s| s.ran = &record);
    let ran = ran.into_inner().unwrap_or_else(|e| e.into_inner());
    let first = ran.first().map_or(0, |r| r.1);
    let between: Vec<(String, Option<usize>)> = ran
        .iter()
        .enumerate()
        .map(|(i, r)| (r.0.0.clone(), r.1.checked_sub(first + i)))
        .collect();
    assert!(
        between.iter().all(|b| b.1 == Some(0)),
        "processes started before each guide command that are not guide commands: {between:?}"
    );
    (got, ran.into_iter().map(|r| r.0).collect())
}

/// The argv of every guide command the check ran, in order: a log kept outside every
/// directory the check owns, so its cleanup cannot remove the evidence.
struct Log(Mutex<Vec<Vec<String>>>);

impl Log {
    fn new() -> Log {
        Log(Mutex::new(Vec::new()))
    }

    fn push(&self, c: &GuideCommand) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(argv(c));
    }

    fn all(&self) -> Vec<Vec<String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Whether a command whose program is `program` ran.
    fn ran(&self, program: &str) -> bool {
        self.all().iter().any(|a| a[0] == program)
    }

    /// Whether anything was extracted, installed or executed from the archive: tar,
    /// install, or a run of an installed `rdy`.
    fn touched_the_archive(&self) -> bool {
        self.all()
            .iter()
            .any(|a| a[0] == "tar" || a[0] == "install" || a[0].ends_with("rdy"))
    }
}

/// [`check_kit`] with the plain stand-ins, each command logged to `log`.
fn check_logged(
    kit: &Kit,
    input: &InstallInput,
    log: &Log,
) -> Result<InstalledRelease, InstallFailure> {
    check_kit(
        kit,
        input,
        &|c| {
            log.push(c);
            stand_in(c, "")
        },
        |_| {},
    )
}

/// Points the symbolic link `link` at `target` in one step: a new link renamed over it.
fn relink(link: &Path, target: &Path) {
    let tmp = link.with_extension("relink");
    platform::symlink(target, &tmp).expect("symlink");
    fs::rename(&tmp, link).expect("rename the link");
}

#[test]
fn pre3_retargeted_download_cannot_substitute_checked_files() {
    if !install_tools_or_skip(&crate::test_name!(
        "pre3_retargeted_download_cannot_substitute_checked_files"
    )) {
        return;
    }
    let target = host_target();
    // The kit's key A signs the manifest, but the trusted key is B. DOWNLOAD is reached
    // through a link, which is pointed, right before OpenSSH runs, at a directory with the
    // same manifest, archive and signature and an allowed-signers file that names A.
    let kit = Kit::new(target);
    let impostor = kit.dir.path().join("impostor");
    fs::create_dir(&impostor).expect("mkdir");
    platform::set_mode(&impostor, 0o700).expect("private");
    for name in [
        MANIFEST_FILE.to_string(),
        SIGNATURE_FILE.to_string(),
        archive_name(IV, target),
    ] {
        fs::copy(kit.download.join(&name), impostor.join(&name)).expect("copy");
    }
    fs::write(impostor.join(SIGNERS_FILE), format!("{}\n", kit.signer)).expect("signers");
    let link = kit.dir.path().join("download link");
    platform::symlink(&kit.download, &link).expect("symlink");
    let mut input = kit.input();
    input.download = link.clone();
    input.signer_line = test_key(&kit.dir.path().join("trusted-key"));
    let log = Log::new();
    let process = |c: &GuideCommand| {
        log.push(c);
        if c.program == "ssh-keygen" {
            relink(&link, &impostor);
        }
        stand_in(c, "")
    };
    assert_eq!(
        check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
        Err(InstallFailure::Signature),
        "the trusted key B did not sign the manifest"
    );
    assert!(!log.touched_the_archive(), "{:?}", log.all());
    kit.assert_untouched(&input, "a retargeted download");

    // The link is pointed at another kit's download, whose archive of the same name holds
    // another binary: while the checksum tool runs, or right before tar runs. tar reads
    // the archive through DOWNLOAD's path, as the guide says, so the check confirms after
    // each command that DOWNLOAD's path still names the checked directory and its checked
    // files. A moved link fails the stage that was running, and neither binary runs or is
    // installed.
    for (when, want) in [
        ("sha256sum", InstallFailure::Checksums),
        ("tar", InstallFailure::Install),
    ] {
        let kit = Kit::new(target);
        let other = Kit::new(target);
        let link = kit.dir.path().join("download link");
        platform::symlink(&kit.download, &link).expect("symlink");
        let mut input = kit.input();
        input.download = link.clone();
        let log = Log::new();
        let process = |c: &GuideCommand| {
            log.push(c);
            let checksum = c.program == "shasum" || c.program == "sha256sum";
            if (when == "sha256sum" && checksum) || c.program == when {
                relink(&link, &other.download);
            }
            stand_in(c, "")
        };
        assert_eq!(
            check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
            Err(want),
            "the link moved at {when}"
        );
        assert!(
            !log.ran("install") && !log.all().iter().any(|a| a[0].ends_with("/rdy")),
            "{when}: nothing installed or run: {:?}",
            log.all()
        );
        assert_eq!(log.ran("tar"), when == "tar", "{when}: {:?}", log.all());
        assert!(other.ran().is_empty(), "{when}: the other binary never ran");
        assert!(kit.ran().is_empty(), "{when}: the checked binary never ran");
        assert!(absent(&input.unpack) && absent(&input.prefix), "{when}");
        assert!(
            absent(&kit.download.join(SIGNERS_FILE)) && absent(&other.download.join(SIGNERS_FILE)),
            "{when}: the signer file is removed, and none is left in the other download"
        );
    }
}

#[test]
fn pre3_openssh_accepts_only_the_trusted_manifest() {
    if !install_tools_or_skip(&crate::test_name!(
        "pre3_openssh_accepts_only_the_trusted_manifest"
    )) {
        return;
    }
    // The trusted key verifies the two-line manifest offline, signed in rdy-release-v1 with
    // sha512, and the release installs.
    let kit = Kit::new(host_target());
    let input = kit.input();
    assert_eq!(kit.lines().len(), 4);
    let before = kit.download_state();
    check_plain(&kit, &input).expect("the trusted manifest");
    assert_eq!(kit.download_state(), before, "the download is unchanged");

    // Each other key, principal, namespace or manifest byte fails at the signature, before
    // any line of the manifest is read.
    type Spoil = fn(&Kit, &mut InstallInput);
    let cases: [(&str, Spoil); 5] = [
        ("another key", |kit, input| {
            input.signer_line = test_key(&kit.dir.path().join("other-key"));
        }),
        ("another namespace", |kit, _| {
            sign(
                &kit.key,
                &kit.download.join(MANIFEST_FILE),
                "rdy-release-v2",
            );
        }),
        ("a signature of other bytes", |kit, _| {
            let other = kit.dir.path().join("other");
            fs::write(&other, b"other bytes\n").expect("write");
            sign(&kit.key, &other, RELEASE_NAMESPACE);
            fs::copy(
                kit.dir.path().join("other.sig"),
                kit.download.join(SIGNATURE_FILE),
            )
            .expect("copy");
        }),
        ("a one-byte change of the manifest", |kit, _| {
            let manifest = kit.download.join(MANIFEST_FILE);
            let mut bytes = fs::read(&manifest).expect("read");
            bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
            fs::write(&manifest, bytes).expect("write");
        }),
        ("a signature that is not SSHSIG", |kit, _| {
            fs::write(kit.download.join(SIGNATURE_FILE), b"not a signature\n").expect("write");
        }),
    ];
    for (what, spoil) in cases {
        let kit = Kit::new(host_target());
        let mut input = kit.input();
        spoil(&kit, &mut input);
        let before = kit.download_state();
        let log = Log::new();
        assert_eq!(
            check_logged(&kit, &input, &log).map(|_| ()),
            Err(InstallFailure::Signature),
            "{what}"
        );
        assert!(!log.touched_the_archive(), "{what}: {:?}", log.all());
        kit.assert_untouched(&input, what);
        assert_eq!(
            kit.download_state(),
            before,
            "{what}: the download is unchanged"
        );
    }

    // The principal: OpenSSH matches the identity to the allowed signer's principal.
    let kit = Kit::new(host_target());
    let input = kit.input();
    let other_principal = |c: &GuideCommand| {
        let mut c = c.clone();
        if c.program == "ssh-keygen" {
            for a in &mut c.args {
                if a == RELEASE_PRINCIPAL {
                    *a = "someone-else".into();
                }
            }
        }
        stand_in(&c, "")
    };
    assert_eq!(
        check_kit(&kit, &input, &other_principal, |_| {}).map(|_| ()),
        Err(InstallFailure::Signature),
        "another principal"
    );
    kit.assert_untouched(&input, "another principal");
    // ... and a signer line naming another principal or namespace is not a release line.
    for line in [
        kit.signer.replacen("rdy-release ", "someone-else ", 1),
        kit.signer.replacen("rdy-release-v1", "rdy-release-v2", 1),
    ] {
        let mut input = kit.input();
        input.signer_line = line;
        assert_eq!(
            check_plain(&kit, &input).map(|_| ()),
            Err(InstallFailure::Input)
        );
    }

    // A missing or empty signature is an input failure.
    for (what, spoil) in [
        (
            "missing",
            (|p: &Path| fs::remove_file(p).expect("rm")) as fn(&Path),
        ),
        ("empty", |p: &Path| fs::write(p, b"").expect("write")),
    ] {
        let kit = Kit::new(host_target());
        let input = kit.input();
        spoil(&kit.download.join(SIGNATURE_FILE));
        assert_eq!(
            check_plain(&kit, &input).map(|_| ()),
            Err(InstallFailure::Input),
            "a {what} signature"
        );
        kit.assert_untouched(&input, what);
    }
}

#[test]
fn pre3_hash_failures_never_extract_or_execute() {
    if !install_tools_or_skip(&crate::test_name!(
        "pre3_hash_failures_never_extract_or_execute"
    )) {
        return;
    }
    let target = host_target();
    let selected = TARGETS.iter().position(|t| *t == target).expect("a target");
    type Spoil = fn(&Kit, usize);
    let cases: [(&str, Spoil, InstallFailure); 10] = [
        (
            "an altered selected archive",
            |kit, _| {
                let a = kit.archive(kit.target);
                let mut bytes = fs::read(&a).expect("read");
                bytes.push(0);
                fs::write(&a, bytes).expect("write");
            },
            InstallFailure::Checksums,
        ),
        (
            "the selected line omitted",
            |kit, sel| {
                let mut lines = kit.lines();
                lines.remove(Kit::at(kit.target));
                let _ = sel;
                kit.write_manifest(lines.concat().as_bytes());
            },
            InstallFailure::Checksums,
        ),
        (
            "a line duplicated",
            |kit, sel| {
                let lines = kit.lines();
                let _ = sel;
                kit.write_manifest(
                    format!("{}{}", lines.concat(), lines[Kit::at(kit.target)]).as_bytes(),
                );
            },
            InstallFailure::Checksums,
        ),
        (
            "the lines reordered",
            |kit, _| {
                let mut lines = kit.lines();
                lines.swap(0, 2);
                kit.write_manifest(lines.concat().as_bytes());
            },
            InstallFailure::Checksums,
        ),
        (
            "a fifth line",
            |kit, _| {
                let mut text = kit.lines().concat();
                text.push_str(&format!("{}  extra.tar.gz\n", "cd".repeat(32)));
                kit.write_manifest(text.as_bytes());
            },
            InstallFailure::Checksums,
        ),
        (
            "CRLF line ends",
            |kit, _| {
                kit.write_manifest(kit.lines().concat().replace('\n', "\r\n").as_bytes());
            },
            InstallFailure::Checksums,
        ),
        (
            "no final LF",
            |kit, _| {
                let text = kit.lines().concat();
                kit.write_manifest(text.trim_end_matches('\n').as_bytes());
            },
            InstallFailure::Checksums,
        ),
        (
            "the selected archive missing",
            |kit, _| fs::remove_file(kit.archive(kit.target)).expect("rm"),
            InstallFailure::Input,
        ),
        (
            "a failed signature",
            |kit, _| {
                let lines = kit.lines();
                fs::write(
                    kit.download.join(MANIFEST_FILE),
                    lines.concat().replace("  ", "   "),
                )
                .expect("write");
            },
            InstallFailure::Signature,
        ),
        (
            "a present nonselected archive that does not match",
            |kit, sel| {
                let lines = kit.lines();
                fs::write(kit.archive(TARGETS[1 - sel]), b"not the archive\n").expect("write");
                kit.write_manifest(lines.concat().as_bytes());
            },
            InstallFailure::Checksums,
        ),
    ];
    for (what, spoil, want) in cases {
        let kit = Kit::new(target);
        let input = kit.input();
        spoil(&kit, selected);
        let before = kit.download_state();
        // the command log is kept outside what the cleanup removes: neither tar nor
        // install nor the binary ever ran
        let log = Log::new();
        assert_eq!(
            check_logged(&kit, &input, &log).map(|_| ()),
            Err(want),
            "{what}"
        );
        assert!(!log.touched_the_archive(), "{what}: {:?}", log.all());
        kit.assert_untouched(&input, what);
        assert_eq!(
            kit.download_state(),
            before,
            "{what}: the download is unchanged"
        );
    }

    // The nonselected archive may be absent (the kit's default) or present and right.
    for present in [false, true] {
        let kit = Kit::new(target);
        if present {
            kit.build_archive(TARGETS[1 - selected], "the other target's binary\n");
            kit.write_manifest(kit.lines().concat().as_bytes());
        }
        let input = kit.input();
        let log = Log::new();
        let report = check_logged(&kit, &input, &log)
            .unwrap_or_else(|f| panic!("present={present}: {}", f.message()));
        assert_eq!(report.skipped, 0);
        assert!(
            log.ran("tar"),
            "the log sees an extraction: {:?}",
            log.all()
        );
        assert_eq!(kit.ran().first().map(String::as_str), Some("--version"));
    }
}

/// A guide command as (program, args, cwd, stdin, stdout) text.
type Shown = (String, Vec<String>, String, Option<String>, Option<String>);

fn shown(c: &GuideCommand) -> Shown {
    let text = |p: &Path| p.to_string_lossy().into_owned();
    (
        c.program.to_string_lossy().into_owned(),
        c.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        text(&c.cwd),
        c.stdin.as_deref().map(text),
        c.stdout.as_deref().map(text),
    )
}

/// A signer line from fixed key bytes.
fn sample_signer() -> String {
    let key = ssh_key::PublicKey::new(
        ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey([7u8; 32])),
        "",
    );
    format!("{SIGNER_PREFIX}{}", key.to_openssh().expect("openssh"))
}

/// What is wrong with the README's trust and install blocks: in the pending state there are
/// none; in the ready state the README holds the generated ready section for its V, built
/// from `keys`: the one key that signs V, the trust list of every key and range, and the
/// install blocks for that key.
fn readme_install_problem(readme: &str, keys: &ReleaseKeys) -> Result<(), String> {
    let generated_marks = [
        "'ssh-keygen' '-Y' 'verify'",
        "### Install\n",
        "### Release keys\n",
        "export PATH=\"<version-prefix>/bin:$PATH\"",
        "'install' '-m' '755'",
    ];
    if readme.contains("Release signing key: pending founder confirmation.") {
        return match generated_marks.iter().find(|m| readme.contains(**m)) {
            Some(m) => Err(format!("the pending state holds an install block: {m}")),
            None => Ok(()),
        };
    }
    let v = readme
        .split("/releases/download/v")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .ok_or("no V in the README")?;
    let notes = format!("https://github.com/adrenickchrome/rdy-core/releases/tag/v{v}");
    let section = readme_ready_section(keys, v, &notes)?;
    if readme.contains(&section) {
        Ok(())
    } else {
        Err("the README's trust and install blocks are not the renderer's".into())
    }
}

#[test]
fn rsig3_generated_install_blocks_stay_exact() {
    let keys = ReleaseKeys::historical();
    let line = keys.signer_for(RV).expect("signer").signer_line;
    // the version wrapper adds nothing to the lower-level renderer
    let section = readme_version_section(&keys, RV).expect("section");
    assert_eq!(section, readme_install_section(RV, &line).expect("section"));
    // four blocks, byte for byte what the renderer prints for the example paths: macOS and
    // Linux in sh, Windows in PowerShell, then the macOS browser block
    assert_eq!(
        section.matches("```sh\n").count(),
        4,
        "three installs and the PATH line"
    );
    assert_eq!(section.matches("```powershell\n").count(), 1);
    let unix = |target: &str, browser: bool| {
        render_commands(
            &commands(&InstallInput {
                download: "/path/to/download".into(),
                signer_line: line.clone(),
                version: RV.into(),
                commit: "0".repeat(40),
                target: target.into(),
                unpack: "/path/to/unpack".into(),
                prefix: format!("/path/to/rdy-{RV}").into(),
                browser,
            })
            .expect("commands"),
        )
        .expect("render")
    };
    let windows = render_guide(&InstallInput {
        download: r"C:\path\to\download".into(),
        signer_line: line.clone(),
        version: RV.into(),
        commit: "0".repeat(40),
        target: WINDOWS_TARGET.into(),
        unpack: r"C:\path\to\unpack".into(),
        prefix: format!(r"C:\path\to\rdy-{RV}").into(),
        browser: false,
    })
    .expect("Windows guide");
    let mut want = String::new();
    for (label, fence, block) in [
        (
            "On macOS arm64, for an archive downloaded with curl:",
            "sh",
            unix(TARGETS[0], false),
        ),
        ("On Linux amd64:", "sh", unix(TARGETS[1], false)),
        ("On Windows amd64, in PowerShell:", "powershell", windows),
        (
            "On macOS arm64, for an archive downloaded with a web browser:",
            "sh",
            unix(TARGETS[0], true),
        ),
    ] {
        want.push_str(&format!("{label}\n\n```{fence}\n{block}\n```\n\n"));
    }
    assert!(section.starts_with("### Install\n\n"));
    let body = &section["### Install\n\n".len()..];
    assert!(body.starts_with(want.trim_end_matches('\n')), "{body}");
    // the plain names, three targets, and no preview name
    for t in [TARGETS[0], WINDOWS_TARGET, TARGETS[1]] {
        assert!(section.contains(&format!("rdy-{RV}-{t}.tar.gz")), "{t}");
    }
    assert!(!section.contains("-preview-"));
    // each block passes the one selected line as one quoted token, in the manifest namespace
    assert_eq!(section.matches(&format!("'{line}'")).count(), 4);
    assert_eq!(section.matches("'-n' 'rdy-release-v1'").count(), 3);
    assert_eq!(
        section
            .matches("-n rdy-release-v1 -s SHA256SUMS.sig")
            .count(),
        1
    );
    // the ready section holds the same install blocks after the trust list
    let ready = readme_ready_section(&keys, RV, "https://example.invalid/notes").expect("ready");
    assert!(ready.ends_with(&section));
    assert!(ready.contains("### Release keys\n"));
    // a version with no key has no section: before v0.17.0, in a gap, a patch release
    for v in ["0.14.0", "0.42.0", "0.20.1", "1.0.0", "v0.20.0"] {
        assert!(readme_version_section(&keys, v).is_err(), "{v}");
        assert!(
            readme_ready_section(&keys, v, "https://example.invalid/notes").is_err(),
            "{v}"
        );
    }
}

#[test]
fn pre3_guide_commands_match_execution() {
    let signer = sample_signer();
    assert!(valid_signer_line(&signer), "{signer}");
    let input = |target: &str, browser: bool| InstallInput {
        download: PathBuf::from("/nowhere/dl dir/it's"),
        signer_line: signer.clone(),
        version: IV.into(),
        commit: COMMIT.into(),
        target: target.into(),
        unpack: PathBuf::from("/nowhere/ü unpack"),
        prefix: PathBuf::from("/nowhere/p p/rdy-0.14.0"),
        browser,
    };
    let dl = "/nowhere/dl dir/it's";
    let stem = |t: &str| format!("rdy-0.14.0-{t}");
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let want = |target: &str, browser: bool| -> Vec<Shown> {
        let archive = format!("{dl}/{}.tar.gz", stem(target));
        let at = |program: &str, args: Vec<String>| -> Shown {
            (program.into(), args, dl.into(), None, None)
        };
        let mut v = vec![
            (
                "printf".into(),
                s(&[r"%s\n", &signer]),
                dl.into(),
                None,
                Some(format!("{dl}/release-allowed-signers")),
            ),
            (
                "ssh-keygen".into(),
                s(&[
                    "-Y",
                    "verify",
                    "-f",
                    "release-allowed-signers",
                    "-I",
                    "rdy-release",
                    "-n",
                    "rdy-release-v1",
                    "-s",
                    "SHA256SUMS.sig",
                ]),
                dl.into(),
                Some(format!("{dl}/SHA256SUMS")),
                None,
            ),
            if target == MAC_TARGET {
                at(
                    "shasum",
                    s(&["-a", "256", "-c", "--ignore-missing", "SHA256SUMS"]),
                )
            } else {
                at("sha256sum", s(&["-c", "--ignore-missing", "SHA256SUMS"]))
            },
        ];
        if browser {
            v.push(at("xattr", s(&["-d", "com.apple.quarantine", &archive])));
        }
        v.extend([
            at(
                "mkdir",
                s(&["/nowhere/ü unpack", "/nowhere/p p/rdy-0.14.0"]),
            ),
            at("tar", s(&["-xzf", &archive, "-C", "/nowhere/ü unpack"])),
            at("mkdir", s(&["-p", "/nowhere/p p/rdy-0.14.0/bin"])),
            at(
                "install",
                s(&[
                    "-m",
                    "755",
                    &format!("/nowhere/ü unpack/{}/rdy", stem(target)),
                    "/nowhere/p p/rdy-0.14.0/bin/rdy",
                ]),
            ),
            at("/nowhere/p p/rdy-0.14.0/bin/rdy", s(&["--version"])),
        ]);
        v
    };
    for (target, browser) in [(TARGETS[0], false), (TARGETS[1], false), (TARGETS[0], true)] {
        let got = commands(&input(target, browser)).expect("commands");
        assert_eq!(
            got.iter().map(shown).collect::<Vec<_>>(),
            want(target, browser),
            "{target} browser={browser}"
        );
    }

    // The rendered block: a cd, then one quoted line per command, each after `&&`.
    let mac =
        render_commands(&commands(&input(TARGETS[0], false)).expect("commands")).expect("render");
    let q = r"'/nowhere/dl dir/it'\''s'";
    let qd = r"/nowhere/dl dir/it'\''s";
    assert_eq!(
        mac,
        format!(
            "cd {q} &&\n\
             'printf' '%s\\n' '{signer}' > '{qd}/release-allowed-signers' &&\n\
             'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '{qd}/SHA256SUMS' &&\n\
             'shasum' '-a' '256' '-c' '--ignore-missing' 'SHA256SUMS' &&\n\
             'mkdir' '/nowhere/ü unpack' '/nowhere/p p/rdy-0.14.0' &&\n\
             'tar' '-xzf' '{qd}/rdy-0.14.0-aarch64-apple-darwin.tar.gz' '-C' '/nowhere/ü unpack' &&\n\
             'mkdir' '-p' '/nowhere/p p/rdy-0.14.0/bin' &&\n\
             'install' '-m' '755' '/nowhere/ü unpack/rdy-0.14.0-aarch64-apple-darwin/rdy' '/nowhere/p p/rdy-0.14.0/bin/rdy' &&\n\
             '/nowhere/p p/rdy-0.14.0/bin/rdy' '--version'"
        )
    );
    for (target, browser) in [(TARGETS[0], false), (TARGETS[1], false), (TARGETS[0], true)] {
        let cmds = commands(&input(target, browser)).expect("commands");
        let block = render_commands(&cmds).expect("render");
        let lines: Vec<&str> = block.lines().filter(|l| !l.starts_with("# ")).collect();
        assert_eq!(lines.len(), cmds.len() + 1, "a cd and one line per command");
        assert!(lines[0].starts_with("cd "));
        let (last, rest) = lines.split_last().expect("lines");
        assert!(
            rest.iter().all(|l| l.ends_with(" &&")),
            "each command runs only after its predecessor succeeded: {block}"
        );
        assert!(!last.ends_with("&&"));
        // verification comes before any extraction, and the quarantine removal before mkdir
        let at = |word: &str| lines.iter().position(|l| l.starts_with(word)).expect(word);
        assert!(at("'ssh-keygen'") < at("'tar'"));
        assert!(at_or(&lines, "'shasum'").min(at_or(&lines, "'sha256sum'")) < at("'tar'"));
        let notes = format!("# {SAFARI_NOTE}\n# {QUARANTINE_NOTE}\n");
        if browser {
            assert!(block.starts_with(&notes), "{block}");
            assert_eq!(at("'xattr'") + 1, at("'mkdir'"));
        } else {
            assert!(!block.contains("xattr") && !block.contains("Safari"));
        }
    }
    // Refused tokens.
    let mut bad = commands(&input(TARGETS[1], false)).expect("commands");
    for token in ["a\nb", "a\rb", "a\0b"] {
        bad[0].args[1] = token.into();
        assert_eq!(
            render_commands(&bad),
            Err(MSG_INVALID_COMMAND.to_string()),
            "{token:?}"
        );
    }

    // Print mode is pure: nonexistent paths are accepted and nothing is made.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut pure = input(TARGETS[1], false);
    pure.download = dir.path().join("no download");
    pure.unpack = dir.path().join("no unpack");
    pure.prefix = dir.path().join("no parent").join("rdy-0.14.0");
    render_commands(&commands(&pure).expect("commands")).expect("render");
    assert!(names(dir.path()).is_empty(), "print mode makes nothing");
    assert_eq!(
        commands(&InstallInput {
            version: "0.14".into(),
            ..pure.clone()
        }),
        Err(MSG_INSTALL_INVALID_ARGUMENTS.to_string())
    );

    // printf's bytes are the signer line and one LF.
    let printf = &commands(&input(TARGETS[1], false)).expect("commands")[0];
    let mut cmd = guide_process(printf);
    cmd.current_dir(dir.path());
    let out = run_bounded(cmd, None, 1 << 20, GUARD).expect("printf");
    assert_eq!(out.stdout, format!("{signer}\n").into_bytes());

    // The README: no install block while the key is pending; the generated ready section
    // (the selected key, the trust list and the install blocks) when ready. A version's key
    // comes from the authenticated records, not from the README.
    let keys = ReleaseKeys::for_repository(&repository_root()).expect("repository trust");
    let readme = fs::read_to_string(repository_root().join("README.md")).expect("README");
    assert_eq!(
        readme_install_problem(&readme, &keys),
        Ok(()),
        "the real README"
    );
    let pending = "Release signing key: pending founder confirmation.\n\
                   Preview downloads are not ready.\n\
                   Do not install a preview until this README and its GitHub release notes show the same key and fingerprint.\n";
    let synthetic =
        format!("# rdy\n\nhttps://github.com/x/releases/download/v{RV}/SHA256SUMS\n\n{pending}");
    assert_eq!(readme_install_problem(&synthetic, &keys), Ok(()));
    let section = readme_version_section(&keys, RV).expect("section");
    assert!(readme_install_problem(&format!("{synthetic}\n{section}"), &keys).is_err());
    let notes = format!("https://github.com/adrenickchrome/rdy-core/releases/tag/v{RV}");
    let ready_section = readme_ready_section(&keys, RV, &notes).expect("ready section");
    let ready = synthetic.replace(pending, &ready_section);
    assert_eq!(readme_install_problem(&ready, &keys), Ok(()));
    // a key that is not the selected one, a missing trust list and a trust list for other
    // records are caught
    let other = keys.signer_for(RV).expect("signer").signer_line;
    assert!(readme_install_problem(&ready.replacen(&other, &sample_signer(), 1), &keys).is_err());
    assert!(
        readme_install_problem(
            &ready.replacen("### Release keys\n", "### Keys\n", 1),
            &keys
        )
        .is_err()
    );
    assert!(readme_install_problem(&ready.replacen("v0.17.0", "v0.18.0", 1), &keys).is_err());
    // the section holds the three blocks for the example paths, in order, each after its
    // label; a browser download on macOS keeps its quarantine mark until the third block
    let mut from = 0;
    for (label, target, browser) in [
        (
            "On macOS arm64, for an archive downloaded with curl:\n\n",
            TARGETS[0],
            false,
        ),
        ("On Linux amd64:\n\n", TARGETS[1], false),
        (
            "On macOS arm64, for an archive downloaded with a web browser:\n\n",
            TARGETS[0],
            true,
        ),
    ] {
        let example = InstallInput {
            download: "/path/to/download".into(),
            unpack: "/path/to/unpack".into(),
            prefix: format!("/path/to/rdy-{RV}").into(),
            version: RV.into(),
            signer_line: other.clone(),
            ..input(target, browser)
        };
        let block = render_commands(&commands(&example).expect("commands")).expect("render");
        let fenced = format!("{label}```sh\n{block}\n```\n");
        let at = section[from..]
            .find(&fenced)
            .unwrap_or_else(|| panic!("{target} browser={browser}: {section}"));
        from += at + fenced.len();
    }
    assert!(section.contains("Add the version's bin directory to PATH in this shell.\n"));
    let edited = ready.replacen("'--version'", "'-V'", 1);
    assert!(
        readme_install_problem(&edited, &keys).is_err(),
        "a hand edit is caught"
    );

    // The check runs these same command objects: the argv, cwd and redirections that its
    // runner starts, recorded by the runner's hook, are the guide's, element for element,
    // on macOS and on Linux. Its other steps (the version receipt, the daemon and MCP
    // checks and the cleanup) come after the guide's last command and are not guide
    // commands.
    if !install_tools_or_skip(&crate::test_name!("pre3_guide_commands_match_execution")) {
        return;
    }
    for target in TARGETS {
        let kit = Kit::new(target);
        let input = kit.input();
        let (got, ran) = check_recorded(&kit, &input, "");
        got.unwrap_or_else(|f| panic!("{target}: {}", f.message()));
        let guide: Vec<Shown> = commands(&input)
            .expect("commands")
            .iter()
            .map(shown)
            .collect();
        assert_eq!(ran, guide, "{target}: the check runs the guide's commands");
        assert!(
            ran.iter().all(|c| c.2 == input.download.to_string_lossy()),
            "{target}: every command starts in DOWNLOAD"
        );
        assert_eq!(
            ran.last().map(|c| c.0.clone()),
            Some(
                input
                    .prefix
                    .join("bin")
                    .join("rdy")
                    .to_string_lossy()
                    .into_owned()
            ),
            "{target}: the product runs by its absolute installed path"
        );
    }
}

/// The position of the first of `lines` that starts with `word`, else past the end.
fn at_or(lines: &[&str], word: &str) -> usize {
    lines
        .iter()
        .position(|l| l.starts_with(word))
        .unwrap_or(usize::MAX)
}

#[test]
fn pre3_browser_quarantine_removal_follows_both_checks() {
    if !install_tools_or_skip(&crate::test_name!(
        "pre3_browser_quarantine_removal_follows_both_checks"
    )) {
        return;
    }
    // Both attribute branches, on the macOS target, with xattr stood in for.
    for (attributes, removes) in [
        (r"com.apple.provenance\ncom.apple.quarantine\n", true),
        (r"com.apple.provenance\n", false),
    ] {
        let kit = Kit::new(MAC_TARGET);
        let mut input = kit.input();
        input.browser = true;
        let (got, ran) = check_recorded(&kit, &input, attributes);
        got.unwrap_or_else(|f| panic!("{}", f.message()));
        // the guide's commands, with the probe of the verified archive right after the
        // checksum command, and the removal only when the probe names the attribute
        let guide = commands(&input).expect("commands");
        let archive = input.download.join(archive_name(IV, MAC_TARGET));
        let probe = GuideCommand::new("xattr", &[archive.as_os_str()], &input.download);
        let mut want: Vec<Shown> = guide.iter().map(shown).collect();
        assert_eq!(want[3].0, "xattr", "the guide's removal");
        assert_eq!(
            want[3].1,
            ["-d", QUARANTINE_ATTRIBUTE, &archive.to_string_lossy()]
        );
        if !removes {
            want.remove(3);
        }
        want.insert(3, shown(&probe));
        assert_eq!(ran, want, "attributes {attributes}");
    }
    // No xattr call before both checks pass.
    for spoil in [
        (|kit: &Kit| {
            let a = kit.archive(MAC_TARGET);
            let mut bytes = fs::read(&a).expect("read");
            bytes.push(0);
            fs::write(&a, bytes).expect("write");
        }) as fn(&Kit),
        |kit: &Kit| fs::write(kit.download.join(SIGNATURE_FILE), b"bad\n").expect("write"),
    ] {
        let kit = Kit::new(MAC_TARGET);
        let mut input = kit.input();
        input.browser = true;
        spoil(&kit);
        let log = Mutex::new(Vec::new());
        let process = |c: &GuideCommand| {
            log.lock().unwrap_or_else(|e| e.into_inner()).push(argv(c));
            stand_in(c, r"com.apple.quarantine\n")
        };
        assert!(check_kit(&kit, &input, &process, |_| {}).is_err());
        let ran = log.into_inner().unwrap_or_else(|e| e.into_inner());
        assert!(ran.iter().all(|a| a[0] != "xattr"), "{ran:?}");
        kit.assert_untouched(&input, "a failed check");
    }
    // A failed probe or removal is an install failure.
    for failing in ["probe", "removal"] {
        let kit = Kit::new(MAC_TARGET);
        let mut input = kit.input();
        input.browser = true;
        let process = |c: &GuideCommand| {
            let removal = c.args.first().is_some_and(|a| a == "-d");
            if c.program == "xattr" && (removal == (failing == "removal")) {
                return shell_script("exit 1");
            }
            stand_in(c, r"com.apple.quarantine\n")
        };
        assert_eq!(
            check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
            Err(InstallFailure::Install),
            "a failed {failing}"
        );
        kit.assert_untouched(&input, failing);
    }
}

#[test]
fn pre3_bounds_and_owned_cleanup() {
    if !install_tools_or_skip(&crate::test_name!("pre3_bounds_and_owned_cleanup")) {
        return;
    }
    use InstallFailure::*;
    let target = host_target();

    // Grammar, each at N and N+1 where it has a bound.
    let kit = Kit::new(target);
    let grammar = |edit: &dyn Fn(&mut InstallInput)| {
        let mut input = kit.input();
        edit(&mut input);
        guide(&input).map(|_| ())
    };
    let long = |len: usize| {
        let tail = "/rdy-0.14.0";
        PathBuf::from(format!("/{}{tail}", "a".repeat(len - 1 - tail.len())))
    };
    assert_eq!(grammar(&|i| i.prefix = long(4096)), Ok(()));
    assert_eq!(grammar(&|i| i.prefix = long(4097)), Err(Input));
    let over = format!(
        "{SIGNER_PREFIX}ssh-ed25519 {}",
        "A".repeat(8193 - SIGNER_PREFIX.len() - 12)
    );
    assert_eq!(over.len(), 8193);
    assert!(!valid_signer_line(&over));
    type Edit = fn(&mut InstallInput);
    let refused: [(&str, Edit); 17] = [
        ("a signer line one byte over", |i| {
            i.signer_line = format!(
                "{SIGNER_PREFIX}ssh-ed25519 {}",
                "A".repeat(8193 - SIGNER_PREFIX.len() - 12)
            );
        }),
        ("a signer comment", |i| i.signer_line.push_str(" me@host")),
        ("a signer of another key type", |i| {
            i.signer_line = i.signer_line.replacen("ssh-ed25519", "ssh-rsa", 1);
        }),
        ("a signer type that disagrees with its key", |i| {
            i.signer_line = i
                .signer_line
                .replacen("ssh-ed25519", "sk-ssh-ed25519@openssh.com", 1);
        }),
        ("a signer with a CR", |i| i.signer_line.push('\r')),
        ("a version of eleven minor digits", |i| {
            i.version = "0.12345678901.0".into()
        }),
        ("a commit of 39 digits", |i| i.commit.truncate(39)),
        ("a commit of 41 digits", |i| i.commit.push('0')),
        ("an uppercase commit", |i| {
            i.commit = i.commit.to_uppercase()
        }),
        ("another target", |i| {
            i.target = "aarch64-unknown-linux-gnu".into()
        }),
        ("a relative download", |i| i.download = "down load".into()),
        ("a prefix not named rdy-V", |i| {
            i.prefix.set_file_name("rdy-0.15.0")
        }),
        ("UNPACK inside DOWNLOAD", |i| {
            i.unpack = i.download.join("x")
        }),
        ("PREFIX inside UNPACK", |i| {
            i.prefix = i.unpack.join("rdy-0.14.0")
        }),
        ("UNPACK as DOWNLOAD, spelled with ..", |i| {
            i.unpack = i
                .download
                .join("..")
                .join(i.download.file_name().expect("name"));
        }),
        ("UNPACK with a NUL", |i| {
            i.unpack = PathBuf::from("/tmp/a\0b")
        }),
        ("the browser path on Linux", |i| {
            i.target = TARGETS[1].into();
            i.browser = true;
        }),
    ];
    let before = kit.download_state();
    for (what, edit) in refused {
        let mut input = kit.input();
        edit(&mut input);
        assert_eq!(check_plain(&kit, &input).map(|_| ()), Err(Input), "{what}");
        kit.assert_untouched(&kit.input(), what);
        assert_eq!(
            kit.download_state(),
            before,
            "{what}: the download is unchanged"
        );
    }

    // A valid signer line at the cap. Its length is fixed by the key type, so the FIDO key's
    // application string sets it: an sk-ssh-ed25519 line is 3 mod 4 bytes long, and 8191
    // and 8195 are the valid lengths on each side of the 8192-byte cap. At the cap the line
    // passes the grammar (and fails the signature: it is not the kit's key); past it, the
    // line is still a well-formed key that only the length refuses.
    let sk_line = |len: usize| -> String {
        let fixed = SIGNER_PREFIX.len() + "sk-ssh-ed25519@openssh.com ".len();
        let base64 = len - fixed;
        assert_eq!(base64 % 4, 0, "{len}");
        // the blob: the type (4 + 26), the key (4 + 32) and the application (4 + its bytes)
        let application = base64 / 4 * 3 - (4 + 26) - (4 + 32) - 4;
        let key = ssh_key::public::SkEd25519::new(
            ssh_key::public::Ed25519PublicKey([9u8; 32]),
            format!("ssh:{}", "a".repeat(application - 4)),
        );
        let key = ssh_key::PublicKey::new(ssh_key::public::KeyData::SkEd25519(key), "");
        let line = format!("{SIGNER_PREFIX}{}", key.to_openssh().expect("openssh"));
        assert_eq!(line.len(), len);
        line
    };
    let (at_cap, past_cap) = (sk_line(SIGNER_LINE_MAX - 1), sk_line(SIGNER_LINE_MAX + 3));
    assert!(valid_signer_line(&at_cap));
    let past_key = past_cap.strip_prefix(SIGNER_PREFIX).expect("prefix");
    assert!(
        ssh_key::PublicKey::from_openssh(past_key)
            .is_ok_and(|k| k.to_openssh().is_ok_and(|again| again == past_key)),
        "past the cap, the key itself is well formed"
    );
    for (line, want) in [(at_cap, Signature), (past_cap, Input)] {
        let kit = Kit::new(target);
        let mut input = kit.input();
        let len = line.len();
        input.signer_line = line;
        assert_eq!(
            check_plain(&kit, &input).map(|_| ()),
            Err(want),
            "a signer line of {len} bytes"
        );
        kit.assert_untouched(&input, "a long signer line");
    }

    // Files in DOWNLOAD: N passes the input checks (and fails the signature), N+1 does not.
    for (name, max) in [
        (MANIFEST_FILE, MANIFEST_MAX),
        (SIGNATURE_FILE, SIGNATURE_MAX),
    ] {
        for (len, want) in [(max, Signature), (max + 1, Input)] {
            let kit = Kit::new(target);
            fs::write(kit.download.join(name), vec![b'x'; len as usize]).expect("write");
            let input = kit.input();
            assert_eq!(
                check_plain(&kit, &input).map(|_| ()),
                Err(want),
                "{name} of {len}"
            );
            kit.assert_untouched(&input, name);
        }
    }
    for (len, want) in [(ARCHIVE_MAX, Signature), (ARCHIVE_MAX + 1, Input)] {
        let kit = Kit::new(target);
        fs::write(kit.download.join(SIGNATURE_FILE), b"bad\n").expect("write");
        let file = File::options()
            .write(true)
            .open(kit.archive(target))
            .expect("open");
        file.set_len(len).expect("sparse archive");
        let input = kit.input();
        assert_eq!(
            check_plain(&kit, &input).map(|_| ()),
            Err(want),
            "an archive of {len}"
        );
        kit.assert_untouched(&input, "archive");
    }

    // Symlinks, a directory, a non-directory DOWNLOAD and a shared one.
    type Fs = fn(&Kit, &InstallInput);
    let fs_cases: [(&str, Fs); 11] = [
        ("a symlinked manifest", |k, _| {
            link_in_place(&k.download.join(MANIFEST_FILE))
        }),
        ("a symlinked signature", |k, _| {
            link_in_place(&k.download.join(SIGNATURE_FILE))
        }),
        ("a symlinked archive", |k, _| {
            link_in_place(&k.archive(k.target))
        }),
        ("a symlinked other archive", |k, _| {
            let other = TARGETS.iter().find(|t| **t != k.target).expect("other");
            platform::symlink(&k.archive(k.target), &k.archive(other)).expect("symlink");
        }),
        ("an archive that is a directory", |k, _| {
            let a = k.archive(k.target);
            fs::remove_file(&a).expect("rm");
            fs::create_dir(&a).expect("mkdir");
        }),
        ("a shared DOWNLOAD", |k, _| {
            platform::set_mode(&k.download, 0o770).expect("chmod")
        }),
        ("an occupied signer file", |k, _| {
            fs::write(k.download.join(SIGNERS_FILE), b"sentinel\n").expect("write");
        }),
        ("an occupied UNPACK", |_, i| {
            fs::create_dir(&i.unpack).expect("mkdir");
            fs::write(i.unpack.join("keep"), b"sentinel\n").expect("write");
        }),
        ("an occupied PREFIX", |_, i| {
            fs::create_dir(&i.prefix).expect("mkdir");
            fs::write(i.prefix.join("keep"), b"sentinel\n").expect("write");
        }),
        ("a dangling link at PREFIX", |k, i| {
            platform::symlink(&k.dir.path().join("nowhere"), &i.prefix).expect("symlink");
        }),
        ("UNPACK under a link to DOWNLOAD", |k, _| {
            platform::symlink(&k.download, &k.dir.path().join("link")).expect("symlink");
        }),
    ];
    for (what, prepare) in fs_cases {
        let kit = Kit::new(target);
        let mut input = kit.input();
        if what == "UNPACK under a link to DOWNLOAD" {
            input.unpack = kit.dir.path().join("link").join("x");
        }
        prepare(&kit, &input);
        let before = kit.download_state();
        let unpack_before = state_of(&input.unpack);
        let prefix_before = state_of(&input.prefix);
        assert_eq!(check_plain(&kit, &input).map(|_| ()), Err(Input), "{what}");
        assert_eq!(
            kit.download_state(),
            before,
            "{what}: the download is unchanged"
        );
        assert_eq!(
            state_of(&input.unpack),
            unpack_before,
            "{what}: UNPACK is unchanged"
        );
        assert_eq!(
            state_of(&input.prefix),
            prefix_before,
            "{what}: PREFIX is unchanged"
        );
        assert!(!kit.marker.exists(), "{what}: nothing ran");
    }
    let kit = Kit::new(target);
    let mut input = kit.input();
    input.unpack = kit.dir.path().join("no parent").join("x");
    assert_eq!(
        check_plain(&kit, &input).map(|_| ()),
        Err(Input),
        "UNPACK without a parent"
    );
    input = kit.input();
    input.download = kit.download.join(MANIFEST_FILE);
    assert_eq!(
        check_plain(&kit, &input).map(|_| ()),
        Err(Input),
        "DOWNLOAD is a file"
    );

    // The machine: another target, an unknown one, or a failed OS probe.
    let kit = Kit::new(target);
    let input = kit.input();
    let other = TARGETS
        .iter()
        .copied()
        .find(|t| *t != target)
        .expect("other");
    let wrong = move || Ok(other);
    let unsupported = || -> io::Result<&'static str> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no release"))
    };
    let no_os = || -> io::Result<String> { Err(io::Error::other("uname failed")) };
    let late_os = || -> io::Result<String> { Err(io::Error::new(io::ErrorKind::TimedOut, "late")) };
    let process = |c: &GuideCommand| stand_in(c, "");
    let results = [
        (
            "another native target",
            Input,
            check_kit(&kit, &input, &process, |s| s.native_target = &wrong),
        ),
        (
            "an unsupported machine",
            Input,
            check_kit(&kit, &input, &process, |s| s.native_target = &unsupported),
        ),
        (
            "a failed OS probe",
            Input,
            check_kit(&kit, &input, &process, |s| s.os = &no_os),
        ),
        (
            "an OS probe past its deadline",
            Deadline,
            check_kit(&kit, &input, &process, |s| s.os = &late_os),
        ),
    ];
    for (what, want, got) in results {
        assert_eq!(got.map(|_| ()), Err(want), "{what}");
        kit.assert_untouched(&input, what);
    }

    // Bounded children: output past the cap, and a child past its deadline (injected).
    let noisy = |c: &GuideCommand| {
        if c.program == "ssh-keygen" {
            return shell_script("head -c 1048577 /dev/zero");
        }
        stand_in(c, "")
    };
    let hanging = |c: &GuideCommand| {
        if c.program == "ssh-keygen" {
            return shell_script("exec sleep 1000");
        }
        stand_in(c, "")
    };
    for (what, want, process, deadline) in [
        (
            "output past the cap",
            OutputLimit,
            &noisy as &dyn Fn(&GuideCommand) -> Command,
            RELEASE_CHILD_DEADLINE,
        ),
        (
            "a child past its deadline",
            Deadline,
            &hanging,
            std::time::Duration::ZERO,
        ),
    ] {
        let kit = Kit::new(target);
        let input = kit.input();
        let before = kit.download_state();
        assert_eq!(
            check_kit(&kit, &input, process, |s| s.deadline = deadline).map(|_| ()),
            Err(want),
            "{what}"
        );
        kit.assert_untouched(&input, what);
        assert_eq!(
            kit.download_state(),
            before,
            "{what}: the download is unchanged"
        );
    }

    // An extracted rdy that is missing, a link, a directory or empty is never installed.
    type Spoil = fn(&Path);
    let extracted: [(&str, Spoil); 4] = [
        ("missing", |rdy| fs::remove_file(rdy).expect("rm")),
        ("a symlink", |rdy| {
            fs::remove_file(rdy).expect("rm");
            platform::symlink(Path::new("/bin/sh"), rdy).expect("symlink");
        }),
        ("a directory", |rdy| {
            fs::remove_file(rdy).expect("rm");
            fs::create_dir(rdy).expect("mkdir");
        }),
        ("empty", |rdy| fs::write(rdy, b"").expect("write")),
    ];
    for (what, spoil) in extracted {
        let kit = Kit::new(target);
        spoil(&kit.source(target).join("rdy"));
        kit.retar(target);
        let input = kit.input();
        assert_eq!(
            check_plain(&kit, &input).map(|_| ()),
            Err(Install),
            "an extracted rdy that is {what}"
        );
        kit.assert_untouched(&input, what);
    }

    // An extracted rdy of exactly the cap is installed and runs (its version is wrong); one
    // byte more is refused before it is installed. Each is the fake `rdy`, ended by `exit
    // 0` and padded with zeros (a sparse file) to its length.
    for (len, want) in [(BINARY_MAX, Version), (BINARY_MAX + 1, Install)] {
        let kit = Kit::new(target);
        let rdy = kit.source(target).join("rdy");
        let script = format!("{}exit 0\n", fake_rdy(&kit.marker, "rdy 0.15.0", 0));
        fs::write(&rdy, script).expect("rdy");
        File::options()
            .write(true)
            .open(&rdy)
            .and_then(|f| f.set_len(len))
            .expect("pad rdy");
        kit.retar(target);
        let input = kit.input();
        let log = Log::new();
        assert_eq!(
            check_logged(&kit, &input, &log).map(|_| ()),
            Err(want),
            "an extracted rdy of {len} bytes"
        );
        if want == Version {
            assert!(log.ran("install"), "the binary at the cap is installed");
            assert_eq!(kit.ran(), ["--version"], "the binary at the cap ran");
        } else {
            assert!(log.ran("tar"), "{:?}", log.all());
            assert!(
                !log.ran("install"),
                "the binary past the cap is refused before install: {:?}",
                log.all()
            );
            assert!(!kit.marker.exists(), "the binary past the cap never ran");
        }
        assert!(absent(&input.unpack) && absent(&input.prefix), "{len}");
        assert!(absent(&kit.download.join(SIGNERS_FILE)));
        assert!(names(&kit.scratch).is_empty());
        fs::remove_file(&rdy).expect("rm the padded source");
    }

    // UNPACK and PREFIX are reached through links, which the installed binary points
    // elsewhere while its version check runs (and fails). The cleanup removes this run's
    // directories through their handles and nothing at the links' new targets.
    let kit = Kit::with_fake(target, &|marker| {
        let dir = marker.parent().expect("the kit's directory");
        let (elsewhere, unpack_alias, prefix_alias) = (
            dir.join("elsewhere"),
            dir.join("unpack alias"),
            dir.join("prefix alias"),
        );
        fake_rdy(marker, "rdy 0.15.0", 0).replacen(
            "#!/bin/sh\n",
            &format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then ln -sfn '{e}' '{u}'; ln -sfn '{e}' '{p}'; fi\n",
                e = elsewhere.display(),
                u = unpack_alias.display(),
                p = prefix_alias.display(),
            ),
            1,
        )
    });
    let mut input = kit.input();
    let unpack_parent = kit.dir.path().join("unpack parent");
    let prefix_parent = kit.dir.path().join("pre fix");
    fs::create_dir(&unpack_parent).expect("mkdir");
    let elsewhere = kit.dir.path().join("elsewhere");
    for d in ["un pack", "rdy-0.14.0"] {
        fs::create_dir_all(elsewhere.join(d)).expect("mkdir");
        fs::write(elsewhere.join(d).join("user-content"), b"sentinel\n").expect("sentinel");
    }
    let unpack_alias = kit.dir.path().join("unpack alias");
    let prefix_alias = kit.dir.path().join("prefix alias");
    platform::symlink(&unpack_parent, &unpack_alias).expect("symlink");
    platform::symlink(&prefix_parent, &prefix_alias).expect("symlink");
    input.unpack = unpack_alias.join("un pack");
    input.prefix = prefix_alias.join("rdy-0.14.0");
    assert_eq!(check_plain(&kit, &input).map(|_| ()), Err(Version));
    assert_eq!(
        fs::read_link(&prefix_alias).ok(),
        Some(elsewhere.clone()),
        "the links were moved"
    );
    for d in ["un pack", "rdy-0.14.0"] {
        assert_eq!(
            fs::read(elsewhere.join(d).join("user-content")).ok(),
            Some(b"sentinel\n".to_vec()),
            "{d} at the link's new target is kept"
        );
    }
    assert!(
        absent(&unpack_parent.join("un pack")) && absent(&prefix_parent.join("rdy-0.14.0")),
        "this run's UNPACK and PREFIX are removed"
    );
    assert_eq!(kit.ran(), ["--version"], "the installed binary ran");

    // Another process makes UNPACK or PREFIX after the input checks found them absent:
    // this run's mkdir of it fails, and that directory is never treated as this run's.
    type Pick = fn(&InstallInput) -> PathBuf;
    let competitors: [(&str, Pick); 2] = [
        ("UNPACK", |i| i.unpack.clone()),
        ("PREFIX", |i| i.prefix.clone()),
    ];
    for (what, pick) in competitors {
        let kit = Kit::new(target);
        let input = kit.input();
        let theirs = pick(&input);
        let log = Log::new();
        let process = |c: &GuideCommand| {
            log.push(c);
            if c.program == "shasum" || c.program == "sha256sum" {
                fs::create_dir(&theirs).expect("the other process's mkdir");
            }
            stand_in(c, "")
        };
        assert_eq!(
            check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
            Err(Install),
            "{what} made by another process"
        );
        assert!(
            theirs.is_dir() && names(&theirs).is_empty(),
            "{what}: the other process's directory stays"
        );
        for ours in [&input.unpack, &input.prefix] {
            if *ours != theirs {
                assert!(absent(ours), "{what}: {} is removed", ours.display());
            }
        }
        assert!(!kit.marker.exists(), "{what}: nothing ran");
        assert!(
            !log.ran("mkdir"),
            "{what}: the guide's mkdir does not run: {:?}",
            log.all()
        );
        assert!(absent(&kit.download.join(SIGNERS_FILE)));
    }
    // ... or right as the guide's `mkdir UNPACK PREFIX` starts. That mkdir fails, so this
    // run claims neither directory: the other process's stays, and so does the one this
    // mkdir may have made (empty), since a failed mkdir does not say which it made.
    for (what, pick) in competitors {
        let kit = Kit::new(target);
        let input = kit.input();
        let theirs = pick(&input);
        let process = |c: &GuideCommand| {
            if c.program == "mkdir" && c.args.first().is_some_and(|a| a != "-p") {
                fs::create_dir(&theirs).expect("the other process's mkdir");
                fs::write(theirs.join("theirs"), b"sentinel\n").expect("their file");
            }
            stand_in(c, "")
        };
        assert_eq!(
            check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
            Err(Install),
            "{what} made as mkdir starts"
        );
        assert_eq!(
            fs::read(theirs.join("theirs")).ok(),
            Some(b"sentinel\n".to_vec()),
            "{what}: the other process's directory stays"
        );
        for ours in [&input.unpack, &input.prefix] {
            if *ours != theirs {
                assert!(
                    absent(ours) || (ours.is_dir() && names(ours).is_empty()),
                    "{what}: {} is absent or empty",
                    ours.display()
                );
            }
        }
        assert!(!kit.marker.exists(), "{what}: nothing ran");
        assert!(absent(&kit.download.join(SIGNERS_FILE)));
    }
    // ... or the link to UNPACK's (or PREFIX's) parent is moved to another directory as
    // that mkdir starts, and another process makes the output in the parent the input
    // checks found. The mkdir succeeds in the new parent. The output's parent path no longer
    // names the checked parent, so this run claims neither directory: the other process's
    // directory (with a file in it or empty) stays, and so do the (empty) directories the
    // mkdir made.
    for (what, their_file) in [
        ("UNPACK", true),
        ("UNPACK", false),
        ("PREFIX", true),
        ("PREFIX", false),
    ] {
        let kit = Kit::new(target);
        let mut input = kit.input();
        let (checked, moved, alias) = (
            kit.dir.path().join("checked parent"),
            kit.dir.path().join("moved parent"),
            kit.dir.path().join("parent alias"),
        );
        for d in [&checked, &moved] {
            fs::create_dir(d).expect("mkdir");
        }
        platform::symlink(&checked, &alias).expect("symlink");
        let output = if what == "UNPACK" {
            &mut input.unpack
        } else {
            &mut input.prefix
        };
        let name = output.file_name().expect("a name").to_os_string();
        *output = alias.join(&name);
        let theirs = checked.join(&name);
        let process = |c: &GuideCommand| {
            if c.program == "mkdir" && c.args.first().is_some_and(|a| a != "-p") {
                fs::remove_file(&alias).expect("rm the link");
                platform::symlink(&moved, &alias).expect("move the link");
                fs::create_dir(&theirs).expect("the other process's mkdir");
                if their_file {
                    fs::write(theirs.join("theirs"), b"sentinel\n").expect("their file");
                }
            }
            stand_in(c, "")
        };
        assert_eq!(
            check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
            Err(Install),
            "{what} ({their_file}): its parent's link moved as mkdir starts"
        );
        assert!(
            theirs.is_dir(),
            "{what} ({their_file}): the other process's directory in the checked parent stays"
        );
        if their_file {
            assert_eq!(
                fs::read(theirs.join("theirs")).ok(),
                Some(b"sentinel\n".to_vec()),
                "{what}: their file stays"
            );
            assert_eq!(names(&theirs), ["theirs"], "{what}: and only their file");
        } else {
            assert!(names(&theirs).is_empty(), "{what}: theirs stays empty");
        }
        for ours in [&input.unpack, &input.prefix] {
            assert!(
                absent(ours) || (ours.is_dir() && names(ours).is_empty()),
                "{what}: {} is absent or empty",
                ours.display()
            );
        }
        assert!(!kit.marker.exists(), "{what}: nothing ran");
        assert!(absent(&kit.download.join(SIGNERS_FILE)));
    }
    // ... or another process removes the directory the mkdir made right after it, and
    // makes its own at that name, with a file in it. The parents are unchanged, but a
    // directory the guide's mkdir just made is empty: this one is not this run's, and
    // neither directory is claimed.
    for what in ["UNPACK", "PREFIX"] {
        let kit = Kit::new(target);
        let input = kit.input();
        let theirs = if what == "UNPACK" {
            input.unpack.clone()
        } else {
            input.prefix.clone()
        };
        let process = |c: &GuideCommand| {
            if c.program == "mkdir" && c.args.first().is_some_and(|a| a != "-p") {
                let q = |p: &Path| format!("'{}'", p.display());
                return shell_script(&format!(
                    "mkdir {u} {p} && rmdir {t} && mkdir {t} && echo sentinel > {f}",
                    u = q(&input.unpack),
                    p = q(&input.prefix),
                    t = q(&theirs),
                    f = q(&theirs.join("theirs")),
                ));
            }
            stand_in(c, "")
        };
        assert_eq!(
            check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
            Err(Install),
            "{what} replaced after mkdir"
        );
        assert_eq!(
            fs::read(theirs.join("theirs")).ok(),
            Some(b"sentinel\n".to_vec()),
            "{what}: the other process's directory stays"
        );
        for ours in [&input.unpack, &input.prefix] {
            if *ours != theirs {
                assert!(
                    ours.is_dir() && names(ours).is_empty(),
                    "{what}: {} the mkdir made stays, empty",
                    ours.display()
                );
            }
        }
        assert!(!kit.marker.exists(), "{what}: nothing ran");
        assert!(absent(&kit.download.join(SIGNERS_FILE)));
    }

    // A failure after the install removes what this run made, and nothing else.
    let kit = Kit::with_rdy(target, "rdy 0.15.0", 0);
    let input = kit.input();
    fs::write(kit.dir.path().join("pre fix").join("keep"), b"sentinel\n").expect("sentinel");
    assert_eq!(check_plain(&kit, &input).map(|_| ()), Err(Version));
    assert_eq!(kit.ran(), ["--version"], "only the version check ran");
    assert!(
        absent(&input.unpack) && absent(&input.prefix),
        "UNPACK and PREFIX are removed"
    );
    assert_eq!(
        names(&kit.dir.path().join("pre fix")),
        ["keep"],
        "PREFIX's parent is kept"
    );
    assert!(absent(&kit.download.join(SIGNERS_FILE)));
    assert!(names(&kit.scratch).is_empty());

    // A failed daemon stop: the scratch project and PREFIX stay for a later stop.
    let kit = Kit::with_rdy(target, "rdy 0.14.0", 1);
    let input = kit.input();
    assert_eq!(check_plain(&kit, &input).map(|_| ()), Err(Cleanup));
    let kept = names(&kit.scratch);
    assert_eq!(kept.len(), 1, "one scratch directory is kept: {kept:?}");
    let project = kit.scratch.join(&kept[0]).join("project");
    assert!(project.is_dir(), "the owned project is kept");
    assert!(
        input.prefix.join("bin").join("rdy").is_file(),
        "PREFIX is kept"
    );
    assert!(absent(&input.unpack), "UNPACK is removed");
    assert!(
        absent(&kit.download.join(SIGNERS_FILE)),
        "the signer file is removed"
    );
    let ran = kit.ran();
    assert_eq!(
        ran.last().map(String::as_str),
        Some(format!("--project {} serve --stop", project.display()).as_str())
    );
}

/// What stands at `p`: nothing, a link and its target, a file's bytes or a directory's tree.
fn state_of(p: &Path) -> String {
    match fs::symlink_metadata(p) {
        Err(_) => "absent".into(),
        Ok(m) if m.file_type().is_symlink() => format!("a link to {:?}", fs::read_link(p)),
        Ok(m) if m.is_dir() => format!("a directory {:?}", tree_of(p)),
        Ok(_) => format!("a file {:?}", fs::read(p)),
    }
}

/// Replaces the file `p` with a symlink to its moved copy.
fn link_in_place(p: &Path) {
    let real = p.with_extension("real");
    fs::rename(p, &real).expect("mv");
    platform::symlink(&real, p).expect("symlink");
}

#[test]
fn pre3_check_installs_and_runs_the_smoke_path() {
    if !install_tools_or_skip(&crate::test_name!(
        "pre3_check_installs_and_runs_the_smoke_path"
    )) {
        return;
    }
    let kit = Kit::new(host_target());
    let input = kit.input();
    let report = check_plain(&kit, &input).unwrap_or_else(|f| panic!("{}", f.message()));
    assert_eq!(report.binary, input.prefix.join("bin").join("rdy"));
    assert_eq!(
        (report.version.as_str(), report.commit.as_str()),
        (IV, COMMIT)
    );
    assert_eq!(report.target, host_target());
    assert_eq!(report.os, platform::release_os().expect("os"));
    assert_eq!(
        report
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.passed))
            .collect::<Vec<_>>(),
        CHECK_NAMES.iter().map(|n| (*n, true)).collect::<Vec<_>>()
    );
    assert_eq!(report.skipped, 0);
    let json = serde_json::to_string(&report).expect("json");
    assert!(json.starts_with("{\"binary\":"), "{json}");
    let keys: Vec<String> = serde_json::from_str::<serde_json::Value>(&json)
        .expect("json")
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect();
    let mut want = [
        "binary", "version", "commit", "target", "os", "checks", "skipped",
    ];
    let in_order = json.match_indices("\":").count();
    assert!(in_order >= want.len());
    want.sort();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(sorted, want);
    // PREFIX holds only bin/rdy, the archive's binary; UNPACK, the signer file and the
    // scratch project are gone.
    assert_eq!(
        tree_of(&input.prefix)
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>(),
        ["bin", "bin/rdy"]
    );
    assert_eq!(
        fs::read(input.prefix.join("bin").join("rdy")).expect("installed"),
        fake_rdy(&kit.marker, "rdy 0.14.0", 0).into_bytes()
    );
    assert_eq!(
        mode_of(&input.prefix.join("bin").join("rdy")).expect("mode"),
        0o755
    );
    assert!(absent(&input.unpack));
    assert!(absent(&kit.download.join(SIGNERS_FILE)));
    assert!(
        names(&kit.scratch).is_empty(),
        "the scratch project is removed"
    );
    // The installed binary ran the version check, then the CLI and MCP paths on one
    // project, then the stop.
    let ran = kit.ran();
    assert_eq!(ran.len(), 5, "{ran:?}");
    assert_eq!(ran[0], "--version");
    let project = ran[1].strip_prefix("init ").expect("init").to_string();
    assert!(
        project.starts_with(&kit.scratch.to_string_lossy().into_owned()),
        "{project}"
    );
    assert_eq!(ran[2], format!("--project {project} --json status"));
    assert_eq!(ran[3], format!("mcp --project {project}"));
    assert_eq!(ran[4], format!("--project {project} serve --stop"));

    // An MCP server that answers every call but then exits nonzero, or by a signal, fails
    // the mcp stage once the daemon is stopped.
    for (what, end) in [("exit 17", "exit 17"), ("a signal", "kill -9 $$")] {
        let kit = Kit::with_fake(host_target(), &|marker| {
            fake_rdy_with(marker, "rdy 0.14.0", 0, end)
        });
        let input = kit.input();
        assert_eq!(
            check_plain(&kit, &input).map(|_| ()),
            Err(InstallFailure::Mcp),
            "{what}"
        );
        let ran = kit.ran();
        assert!(
            ran.last().is_some_and(|l| l.ends_with("serve --stop")),
            "{what}: the daemon is stopped: {ran:?}"
        );
        assert!(absent(&input.unpack) && absent(&input.prefix), "{what}");
        assert!(names(&kit.scratch).is_empty(), "{what}: no scratch project");
    }
}

#[test]
fn relc1_four_entry_manifest_verifies_before_extract() {
    if !install_tools_or_skip(&crate::test_name!(
        "relc1_four_entry_manifest_verifies_before_extract"
    )) {
        return;
    }
    let target = host_target();
    // the four-line manifest of the plain names installs
    let kit = Kit::new(target);
    assert_eq!(
        kit.lines()
            .iter()
            .map(|l| l.split("  ").nth(1).expect("name").trim_end().to_string())
            .collect::<Vec<_>>(),
        manifest_names(IV)
    );
    check_plain(&kit, &kit.input()).expect("the four-line manifest");
    // the three archive lines with no sample install too: Release 1 (0.47.0) ships no sample
    let kit = Kit::new(target);
    kit.write_manifest(kit.lines()[..3].concat().as_bytes());
    check_plain(&kit, &kit.input()).expect("the three-line manifest with no sample");

    // A manifest that OpenSSH accepts but whose grammar is wrong stops the run as `checksums`,
    // after the signature and before any extraction, install or run.
    type Spoil = fn(&Kit);
    let cases: [(&str, Spoil); 7] = [
        ("the historical two-line manifest", |kit| {
            let lines = kit.lines();
            kit.write_manifest(format!("{}{}", lines[0], lines[2]).as_bytes());
        }),
        ("an archive line missing", |kit| {
            let lines = kit.lines();
            kit.write_manifest(format!("{}{}{}", lines[0], lines[2], lines[3]).as_bytes());
        }),
        ("the sample line before the archives", |kit| {
            let mut lines = kit.lines();
            lines.rotate_right(1);
            kit.write_manifest(lines.concat().as_bytes());
        }),
        ("a preview archive name", |kit| {
            let text = kit.lines().concat();
            let name = archive_name(IV, kit.target);
            let preview = format!("rdy-{IV}-preview-{}.tar.gz", kit.target);
            kit.write_manifest(text.replace(&name, &preview).as_bytes());
        }),
        ("a path in the sample line", |kit| {
            let text = kit.lines().concat();
            kit.write_manifest(
                text.replace(SAMPLE_FILE, "samples/tally.rdydump")
                    .as_bytes(),
            );
        }),
        ("an extra file", |kit| {
            let mut text = kit.lines().concat();
            text.push_str(&format!("{}  notes.txt\n", "cd".repeat(32)));
            kit.write_manifest(text.as_bytes());
        }),
        ("CRLF line ends", |kit| {
            kit.write_manifest(kit.lines().concat().replace('\n', "\r\n").as_bytes());
        }),
    ];
    for (what, spoil) in cases {
        let kit = Kit::new(target);
        let input = kit.input();
        spoil(&kit);
        let before = kit.download_state();
        let log = Log::new();
        assert_eq!(
            check_logged(&kit, &input, &log).map(|_| ()),
            Err(InstallFailure::Checksums),
            "{what}"
        );
        assert!(!log.touched_the_archive(), "{what}: {:?}", log.all());
        kit.assert_untouched(&input, what);
        assert_eq!(kit.download_state(), before, "{what}: DOWNLOAD unchanged");
    }
}

#[test]
fn relc1_sample_hash_is_separate_from_six_install_checks() {
    if !install_tools_or_skip(&crate::test_name!(
        "relc1_sample_hash_is_separate_from_six_install_checks"
    )) {
        return;
    }
    let target = host_target();
    let names = |r: &InstalledRelease| {
        r.checks
            .iter()
            .map(|c| (c.name.clone(), c.passed))
            .collect::<Vec<_>>()
    };
    let six: Vec<(String, bool)> = CHECK_NAMES.iter().map(|n| (n.to_string(), true)).collect();

    // no sample beside the archive: the six checks, no seventh
    let kit = Kit::new(target);
    let report = check_plain(&kit, &kit.input()).expect("no sample");
    assert_eq!((names(&report), report.skipped), (six.clone(), 0));

    // a sample that matches the manifest is hashed by the checksum tool and adds no check
    let kit = Kit::new(target);
    kit.add_sample(b"a stand-in for the frozen sample dump\n");
    let log = Log::new();
    let report = check_logged(&kit, &kit.input(), &log).expect("a good sample");
    assert_eq!((names(&report), report.skipped), (six, 0));
    assert!(log.ran("tar"), "{:?}", log.all());

    // a sample that does not match its manifest line stops the run at `checksums`, before any
    // extraction, install or run
    let kit = Kit::new(target);
    kit.add_sample(b"a stand-in for the frozen sample dump\n");
    fs::write(kit.download.join(SAMPLE_FILE), b"another dump\n").expect("write");
    let input = kit.input();
    let before = kit.download_state();
    let log = Log::new();
    assert_eq!(
        check_logged(&kit, &input, &log).map(|_| ()),
        Err(InstallFailure::Checksums)
    );
    assert!(!log.touched_the_archive(), "{:?}", log.all());
    kit.assert_untouched(&input, "a changed sample");
    assert_eq!(kit.download_state(), before);

    // a sample over its bound, or one that is not a regular file, is refused as input
    let kit = Kit::new(target);
    let big = fs::File::create(kit.download.join(SAMPLE_FILE)).expect("sample");
    big.set_len(SAMPLE_MAX + 1).expect("sparse");
    drop(big);
    let input = kit.input();
    assert_eq!(
        check_plain(&kit, &input).map(|_| ()),
        Err(InstallFailure::Input)
    );
    fs::remove_file(kit.download.join(SAMPLE_FILE)).expect("rm");
    fs::create_dir(kit.download.join(SAMPLE_FILE)).expect("mkdir");
    assert_eq!(
        check_plain(&kit, &input).map(|_| ()),
        Err(InstallFailure::Input)
    );
}
