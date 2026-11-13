//! rfr4 (D-81YJG, D-QGN3A; the O5 release assembly contract): `qa-release-readme` rebuilds the
//! repository README from the checked export `release/readme.md`, its sidecar
//! `release/readme.sha256` and the exported images in `assets/readme/`. The four generated parts
//! (Archives, Release signing key, Release keys, Install) go in once, before the final verify
//! fold's close. `--check` compares the README with the same assembly and writes nothing.
//!
//! These tests use synthetic roots only. Each root holds a copy of the repository's public,
//! authenticated `release-keys/` records, so the real root check runs. The synthetic key epochs
//! of the library tests use a structural mock that proves no signature.
use rdy_qa::fixtures::output_within;
use rdy_qa::release_install::{
    TARGETS, WINDOWS_TARGET, archive_name, readme_archives_section, readme_install_section,
    readme_notes_url, readme_ready_section,
};
use rdy_qa::release_keys::{self, ReleaseKeys, StatementCheck};
use rdy_qa::release_readme::{self, Limits, Mode, ReadmeFailure};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The version of every synthetic root: inside the repository's current machine epoch.
const V: &str = "0.50.0";

/// The verify fold's authored intro, as the export writes it.
const VERIFY_INTRO: &str = "You do not need this to use rdy. It shows how to check that an archive is the one we signed, before you install it.\n";

const ABOUT_OPEN: &str = "<details>\n<summary><b>About Release 1</b></summary>\n";
const VERIFY_OPEN: &str = "<details>\n<summary><b>Verify the download (optional)</b></summary>\n";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A generic picture's bytes: a PNG signature and a distinct tail.
fn picture(tail: &str) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(tail.as_bytes());
    b
}

fn png_name(bytes: &[u8]) -> String {
    format!("{}.png", sha(bytes))
}

/// The synthetic export: `body` after the intro, then the two final folds. `verify` is the
/// verify fold's body after its intro.
fn source_of(body: &str, verify: &str) -> String {
    format!(
        "# rdy\n\nIntro words.\n\n{body}## Report problems\n\nReport words.\n\n{ABOUT_OPEN}\nAbout words.\n\n</details>\n\n{VERIFY_OPEN}\n{VERIFY_INTRO}\n### Platforms\n\n| Platform | Status |\n|---|---|\n| macOS (arm64) | Supported. |\n{verify}\n</details>\n"
    )
}

/// The plain synthetic export with no image.
fn plain_source() -> String {
    source_of("", "")
}

/// The three images of [`image_source`]: plain, light and dark.
fn images() -> Vec<Vec<u8>> {
    vec![
        picture("plain image bytes"),
        picture("light image bytes"),
        picture("dark image bytes"),
    ]
}

/// A source with a plain figure, a generated picture with its dark `srcset`, and a remote badge.
fn image_source() -> String {
    let [p, l, d] = [0, 1, 2].map(|i| png_name(&images()[i]));
    source_of(
        &format!(
            "![Plain figure](assets/readme/{p})\n\n<picture>\n<source media=\"(prefers-color-scheme: dark)\" srcset=\"assets/readme/{d}\">\n<img alt=\"Paired\" src=\"assets/readme/{l}\">\n</picture>\n\nBadge ![Build](https://example.com/badge.svg) stays.\n\n"
        ),
        "",
    )
}

/// The image files of [`image_source`], by name.
fn image_files() -> Vec<(String, Vec<u8>)> {
    images().into_iter().map(|b| (png_name(&b), b)).collect()
}

/// The repository's authenticated trust records.
fn real_keys() -> ReleaseKeys {
    ReleaseKeys::load(&repo().join("release-keys")).expect("the repository's release keys")
}

/// The four generated parts for `v` under `keys`.
fn generated(keys: &ReleaseKeys, v: &str) -> String {
    format!(
        "{}\n{}",
        readme_archives_section(v),
        readme_ready_section(keys, v, &readme_notes_url(v)).expect("ready section")
    )
}

/// The expected README of `source`: the generated parts and one LF before the final close.
fn expected(source: &str, keys: &ReleaseKeys, v: &str) -> String {
    let pos = source.rfind("</details>\n").expect("the final close");
    format!(
        "{}{}\n{}",
        &source[..pos],
        generated(keys, v),
        &source[pos..]
    )
}

// --- synthetic roots ----------------------------------------------------------------------------

/// A synthetic repository root: workspace version `workspace`, a copy of the real release keys,
/// the source `source` (as bytes) with the sidecar `sidecar`, the images `files`, and the README
/// `readme`.
struct RootSpec<'a> {
    workspace: &'a str,
    source: Vec<u8>,
    sidecar: Option<Vec<u8>>,
    files: Vec<(String, Vec<u8>)>,
    readme: &'a [u8],
}

impl RootSpec<'_> {
    fn of(source: &str) -> Self {
        RootSpec {
            workspace: V,
            source: source.as_bytes().to_vec(),
            sidecar: None,
            files: vec![],
            readme: b"The old README.\n",
        }
    }

    fn bytes(source: Vec<u8>) -> Self {
        RootSpec {
            source,
            ..RootSpec::of("")
        }
    }

    fn with_files(mut self, files: Vec<(String, Vec<u8>)>) -> Self {
        self.files = files;
        self
    }

    fn build(&self) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"{}\"\n",
                self.workspace
            ),
        )
        .unwrap();
        let keys = root.join("release-keys");
        std::fs::create_dir(&keys).unwrap();
        for e in std::fs::read_dir(repo().join("release-keys")).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), keys.join(e.file_name())).unwrap();
        }
        std::fs::create_dir(root.join("release")).unwrap();
        std::fs::write(root.join("release/readme.md"), &self.source).unwrap();
        let sidecar = self
            .sidecar
            .clone()
            .unwrap_or_else(|| format!("{}\n", sha(&self.source)).into_bytes());
        std::fs::write(root.join("release/readme.sha256"), sidecar).unwrap();
        if !self.files.is_empty() {
            std::fs::create_dir_all(root.join("assets/readme")).unwrap();
            for (name, bytes) in &self.files {
                std::fs::write(root.join("assets/readme").join(name), bytes).unwrap();
            }
        }
        std::fs::write(root.join("README.md"), self.readme).unwrap();
        dir
    }
}

/// Every entry under `dir` with its bytes (`None` for a directory).
fn snapshot(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            let kind = std::fs::symlink_metadata(&p).unwrap().file_type();
            if kind.is_symlink() {
                // a link is its target's path, never followed
                let target = std::fs::read_link(&p).unwrap();
                out.insert(rel, Some(format!("-> {}", target.display()).into_bytes()));
            } else if kind.is_dir() {
                out.insert(format!("{rel}/"), None);
                walk(base, &p, out);
            } else if kind.is_file() {
                out.insert(rel, Some(std::fs::read(&p).unwrap()));
            } else {
                // a FIFO or another special file is never opened
                out.insert(rel, Some(format!("{kind:?}").into_bytes()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Runs `qa-release-readme` with `args`, offline: a cleared environment, an empty `PATH` (no
/// Git, no tool at all), and a home directory that must stay empty.
fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let home = tempfile::tempdir().unwrap();
    let path = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qa-release-readme"));
    cmd.env_clear()
        .env("PATH", path.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("http_proxy", "http://127.0.0.1:9")
        .env("https_proxy", "http://127.0.0.1:9")
        .current_dir(home.path())
        .args(args);
    if let Some(root) = std::env::var_os("SYSTEMROOT") {
        cmd.env("SYSTEMROOT", root);
    }
    let out = output_within(&mut cmd, Duration::from_secs(300)).expect("run qa-release-readme");
    assert!(
        std::fs::read_dir(home.path()).unwrap().next().is_none(),
        "the command wrote to its home directory"
    );
    assert!(
        std::fs::read_dir(path.path()).unwrap().next().is_none(),
        "the PATH directory is untouched"
    );
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Runs the command on `root` for `version`, in check mode when `check`.
fn run_on(root: &Path, version: &str, check: bool) -> (Option<i32>, String, String) {
    let root = root.to_str().expect("a UTF-8 temp path");
    let mut args = vec!["--root", root, "--version", version];
    if check {
        args.push("--check");
    }
    run(&args)
}

/// Both modes refuse `root` with exit `code` and exactly `line` and LF on stderr, print nothing
/// on stdout, and change no file.
fn refuses_root(root: &Path, version: &str, code: i32, line: &str) {
    for check in [false, true] {
        let before = snapshot(root);
        let (got, stdout, stderr) = run_on(root, version, check);
        assert_eq!(
            (got, stdout.as_str(), stderr.as_str()),
            (Some(code), "", format!("{line}\n").as_str()),
            "check {check}"
        );
        assert_eq!(snapshot(root), before, "check {check}: no file changed");
    }
}

/// [`refuses_root`] on a fresh root of `spec`.
fn refuses(spec: &RootSpec, code: i32, line: &str) {
    let dir = spec.build();
    refuses_root(dir.path(), V, code, line);
}

/// Writes the README of `root`; the command succeeds silently and changes README.md alone.
fn writes(root: &Path) -> Vec<u8> {
    let before = snapshot(root);
    assert_eq!(
        run_on(root, V, false),
        (Some(0), String::new(), String::new())
    );
    let mut after = snapshot(root);
    let readme = after
        .insert("README.md".into(), before["README.md"].clone())
        .flatten()
        .expect("README.md");
    assert_eq!(
        after, before,
        "only README.md changed, and no other file is left"
    );
    readme
}

// --- the structural mock ----------------------------------------------------------------------

/// STRUCTURAL MOCK: accepts every record without any cryptography. The library tests check the
/// README's shape against synthetic epochs, never a signature.
struct Structural;

impl StatementCheck for Structural {
    fn check(&self, _: &str, _: &[u8], _: &[u8]) -> bool {
        true
    }
}

const ARMOR: &[u8] =
    b"-----BEGIN SSH SIGNATURE-----\nU1NIU0lHAAAAAQ==\n-----END SSH SIGNATURE-----\n";
const EVIDENCE: &[u8] = b"{\"approval_channel\":null,\"coordinators\":[],\"evidence_workflow\":{},\"release_workflow_path\":\".github/workflows/release.yml\",\"repository_id\":1403751388,\"schema\":1}\n";

fn key_of(byte: u8) -> ssh_key::PublicKey {
    ssh_key::PublicKey::new(
        ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey([byte; 32])),
        "",
    )
}

/// Disposable machine epochs, each `(first, last good, key byte)`, under the structural mock.
fn machine_keys(epochs: &[(u32, Option<u32>, u8)]) -> ReleaseKeys {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, bytes: &[u8]| std::fs::write(dir.path().join(name), bytes).unwrap();
    let mut names = vec![];
    let mut last_fp = String::new();
    for (first, last, byte) in epochs {
        let pk = key_of(*byte);
        let key = pk.to_openssh().unwrap();
        let fp = pk.fingerprint(ssh_key::HashAlg::Sha256).to_string();
        let t = format!("transition-v0.{first}.0.json");
        write(&t, format!("{{\n  \"fingerprint\": \"{fp}\",\n  \"first_machine_tag\": \"v0.{first}.0\",\n  \"public_key\": \"{key}\",\n  \"repository\": \"adrenickchrome/rdy-core\",\n  \"scheme\": \"rdy-release-signer-transition-v1\"\n}}\n").as_bytes());
        write(&format!("{t}.sig"), ARMOR);
        names.push(t);
        if let Some(last) = last {
            let r = format!("revocation-v0.{first}.0.json");
            write(&r, format!("{{\n  \"fingerprint\": \"{fp}\",\n  \"first_machine_tag\": \"v0.{first}.0\",\n  \"last_good_tag\": \"v0.{last}.0\",\n  \"repository\": \"adrenickchrome/rdy-core\",\n  \"scheme\": \"rdy-release-signer-revocation-v1\"\n}}\n").as_bytes());
            write(&format!("{r}.sig"), ARMOR);
            names.push(r);
        }
        last_fp = fp;
    }
    let records = release_keys::records_index(names.iter().map(String::as_str));
    write(release_keys::RECORDS_FILE, records.as_bytes());
    write(release_keys::EVIDENCE_POLICY_FILE, EVIDENCE);
    let first = epochs.last().unwrap().0;
    write(
        release_keys::POLICY_FILE,
        format!(
            "{{\"evidence_policy_sha256\":\"{}\",\"fingerprint\":\"{last_fp}\",\"first_machine_tag\":\"v0.{first}.0\",\"key_arn\":\"arn:aws:kms:us-east-1:000000000000:key/00000000-0000-0000-0000-000000000000\",\"records_sha256\":\"{}\",\"repository\":\"adrenickchrome/rdy-core\",\"role_arn\":\"arn:aws:iam::000000000000:role/synthetic-test-role\",\"schema\":1,\"signer_workflow_sha\":\"0000000000000000000000000000000000000000\",\"targets\":[\"aarch64-apple-darwin\",\"x86_64-unknown-linux-gnu\"]}}\n",
            release_keys::sha256_hex(EVIDENCE),
            release_keys::sha256_hex(records.as_bytes()),
        )
        .as_bytes(),
    );
    ReleaseKeys::load_checked_by(dir.path(), &Structural).expect("synthetic machine records")
}

fn signer_line_of(byte: u8) -> String {
    format!(
        "rdy-release namespaces=\"rdy-release-v1\" {}",
        key_of(byte).to_openssh().unwrap()
    )
}

// --- 1. one insertion -----------------------------------------------------------------------------

#[test]
fn rfr4_readme_inserts_generated_parts_once() {
    let keys = real_keys();
    let source = plain_source();
    let out = release_readme::assemble(source.as_bytes(), &keys, V).expect("assembled");

    // the exact source before the final close, the four parts with one LF, then the close
    let pos = source.rfind("</details>\n").unwrap();
    assert!(out.starts_with(&source[..pos]), "the source prefix is kept");
    assert!(
        out.ends_with("\n\n</details>\n"),
        "a blank line before the close"
    );
    assert_eq!(
        out,
        format!("{}{}\n</details>\n", &source[..pos], generated(&keys, V))
    );
    assert_eq!(out, expected(&source, &keys, V));

    // four ordered headings, each once; one trust section; LF only and one final LF
    let at = |h: &str| -> usize {
        assert_eq!(out.matches(&format!("\n{h}\n")).count(), 1, "{h} once");
        out.find(&format!("\n{h}\n")).unwrap()
    };
    let order = [
        at("### Platforms"),
        at("### Archives"),
        at("### Release signing key"),
        at("### Release keys"),
        at("### Install"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    assert_eq!(
        out.matches("Each version has exactly one release key.")
            .count(),
        1
    );
    assert!(!out.contains('\r'));
    assert!(out.ends_with("</details>\n") && !out.ends_with("\n\n"));
    // the generated parts sit inside the verify fold, after the About Release 1 fold
    assert!(out.find(ABOUT_OPEN).unwrap() < out.find(VERIFY_OPEN).unwrap());
    assert!(out.find(VERIFY_OPEN).unwrap() < order[1]);

    // a close inside fenced code is not the fold's close: the parts go before the real close
    let fenced = source_of("", "\n```text\n</details>\n```\n");
    let out = release_readme::assemble(fenced.as_bytes(), &keys, V).expect("fenced close");
    assert_eq!(out, expected(&fenced, &keys, V));
    assert!(out.contains("```text\n</details>\n```\n\n### Archives\n"));

    // nested folds before the verify fold, and inside it, are kept as they are
    let nested = source_of(
        "<details>\n<summary><b>Outer</b></summary>\n\n<details>\n<summary><b>Inner</b></summary>\n\nWords.\n\n</details>\n\n</details>\n\n",
        "\n<details>\n<summary><b>A note</b></summary>\n\nNote words.\n\n</details>\n",
    );
    let out = release_readme::assemble(nested.as_bytes(), &keys, V).expect("nested");
    assert_eq!(out, expected(&nested, &keys, V));

    // the command writes the same bytes into README.md
    let dir = RootSpec::of(&source).build();
    assert_eq!(
        writes(dir.path()),
        expected(&source, &keys, V).into_bytes(),
        "the written README"
    );
    // and the source with images, its assets checked
    let dir = RootSpec::of(&image_source())
        .with_files(image_files())
        .build();
    assert_eq!(
        writes(dir.path()),
        expected(&image_source(), &keys, V).into_bytes()
    );
}

// --- 2. targets, version and keys -----------------------------------------------------------------

/// The Archives part for 0.50.0, byte for byte.
const ARCHIVES_0_50_0: &str = "### Archives

Each release has three archives: macOS arm64, Windows amd64 and Linux amd64. The Linux archive is qualified on Ubuntu 26.

| Platform | Target | Archive |
|---|---|---|
| macOS arm64 | `aarch64-apple-darwin` | [rdy-0.50.0-aarch64-apple-darwin.tar.gz](https://github.com/adrenickchrome/rdy-core/releases/download/v0.50.0/rdy-0.50.0-aarch64-apple-darwin.tar.gz) |
| Windows amd64 | `x86_64-pc-windows-msvc` | [rdy-0.50.0-x86_64-pc-windows-msvc.tar.gz](https://github.com/adrenickchrome/rdy-core/releases/download/v0.50.0/rdy-0.50.0-x86_64-pc-windows-msvc.tar.gz) |
| Linux amd64 | `x86_64-unknown-linux-gnu` | [rdy-0.50.0-x86_64-unknown-linux-gnu.tar.gz](https://github.com/adrenickchrome/rdy-core/releases/download/v0.50.0/rdy-0.50.0-x86_64-unknown-linux-gnu.tar.gz) |

The release is [v0.50.0](https://github.com/adrenickchrome/rdy-core/releases/tag/v0.50.0). It also holds [SHA256SUMS](https://github.com/adrenickchrome/rdy-core/releases/download/v0.50.0/SHA256SUMS) and [SHA256SUMS.sig](https://github.com/adrenickchrome/rdy-core/releases/download/v0.50.0/SHA256SUMS.sig).

Download the archive for your platform, SHA256SUMS and SHA256SUMS.sig into one new directory.
Check the signature with OpenSSH, then check the archive hash, before extraction or execution. The signed file is SHA256SUMS.
This signature is not Apple notarization.
";

#[test]
fn rfr4_readme_uses_release_targets_version_and_authenticated_keys() {
    // the Archives part: the current targets through the archive naming function
    assert_eq!(readme_archives_section("0.50.0"), ARCHIVES_0_50_0);
    for v in ["0.51.0", "0.60.0"] {
        let part = readme_archives_section(v);
        for target in [TARGETS[0], WINDOWS_TARGET, TARGETS[1]] {
            let name = archive_name(v, target);
            assert!(
                part.contains(&format!(
                    "[{name}](https://github.com/adrenickchrome/rdy-core/releases/download/v{v}/{name})"
                )),
                "{v} {target}"
            );
        }
        assert_eq!(part.matches(".tar.gz](").count(), 3, "three archives");
        assert_eq!(part.matches(&format!("v{v}")).count(), 7, "{v} only");
    }
    assert_eq!(
        readme_notes_url("0.50.0"),
        "https://github.com/adrenickchrome/rdy-core/releases/tag/v0.50.0"
    );

    // the selected signer per version, every epoch in the trust list, the install generator's
    // own bytes
    let two = machine_keys(&[(50, Some(55), 7), (60, None, 8)]);
    let source = plain_source();
    for (v, line) in [
        (
            "0.35.0",
            release_keys::ROOT_ALLOWED_SIGNERS.trim_end().to_string(),
        ),
        ("0.50.0", signer_line_of(7)),
        ("0.55.0", signer_line_of(7)),
        ("0.60.0", signer_line_of(8)),
    ] {
        let out = release_readme::assemble(source.as_bytes(), &two, v).expect(v);
        assert_eq!(out, expected(&source, &two, v), "{v}");
        assert!(
            out.contains(&format!(
                "### Release signing key\n\n```text\n{line}\n```\n"
            )),
            "{v}: the selected key"
        );
        assert!(
            out.contains(&readme_install_section(v, &line).unwrap()),
            "{v}: the install generator's bytes"
        );
        for row in [
            "| v0.17.0 to v0.41.0 | `sk-ssh-ed25519@openssh.com` |",
            "| v0.50.0 to v0.55.0 | `ssh-ed25519` |",
            "| v0.60.0 and later | `ssh-ed25519` |",
        ] {
            assert!(out.contains(row), "{v}: {row}");
        }
        assert!(out.contains(&readme_archives_section(v)), "{v}: archives");
    }
    // a version with no key has no README
    for v in ["0.16.0", "0.56.0", "0.42.0"] {
        assert_eq!(
            release_readme::assemble(source.as_bytes(), &two, v),
            Err(ReadmeFailure::Keys),
            "{v}"
        );
    }
    // the real records: the historical key and the current machine key
    let real = release_readme::assemble(source.as_bytes(), &real_keys(), V).unwrap();
    assert!(real.contains("| v0.17.0 to v0.41.0 | `sk-ssh-ed25519@openssh.com` |"));
    assert!(real.contains("| v0.46.0 and later | `ssh-ed25519` |"));

    // the command: V must equal the workspace version
    let spec = RootSpec::of(&source);
    let dir = spec.build();
    for other in ["0.49.0", "0.51.0", "1.50.0"] {
        refuses_root(
            dir.path(),
            other,
            1,
            "release readme: version does not match the workspace",
        );
    }
    refuses(
        &RootSpec {
            workspace: "0.51.0",
            ..RootSpec::of(&source)
        },
        1,
        "release readme: version does not match the workspace",
    );

    // the release keys: missing, refused by the reader, or no signer for V
    let keys_invalid = "release readme: release keys are invalid";
    let dir = spec.build();
    std::fs::remove_dir_all(dir.path().join("release-keys")).unwrap();
    refuses_root(dir.path(), V, 1, keys_invalid);
    let dir = spec.build();
    let t = dir.path().join("release-keys/transition-v0.46.0.json");
    let mut bytes = std::fs::read(&t).unwrap();
    let last = bytes.len() - 3;
    bytes[last] ^= 1;
    std::fs::write(&t, bytes).unwrap();
    refuses_root(dir.path(), V, 1, keys_invalid);
    let dir = spec.build();
    std::fs::write(dir.path().join("release-keys/stray.txt"), b"x\n").unwrap();
    refuses_root(dir.path(), V, 1, keys_invalid);
    for workspace in ["0.42.0", "0.16.0"] {
        let dir = RootSpec {
            workspace,
            ..RootSpec::of(&source)
        }
        .build();
        refuses_root(dir.path(), workspace, 1, keys_invalid);
    }

    // the arguments
    let dir = spec.build();
    let root = dir.path().to_str().unwrap();
    let before = snapshot(dir.path());
    for args in [
        vec![],
        vec!["--root", root],
        vec!["--version", V],
        vec!["--root", root, "--version", V, "--bogus"],
        vec!["--root", root, "--version", V, "extra"],
        vec!["--root", root, "--root", root, "--version", V],
        vec!["--root", root, "--version", V, "--version", V],
        vec!["--root", root, "--version", V, "--check", "--check"],
        vec!["--root", root, "--version", "0.50"],
        vec!["--root", root, "--version", "0.50.0.1"],
        vec!["--root", root, "--version", "v0.50.0"],
        vec!["--root", root, "--version", "0.50.x"],
        vec!["--root", root, "--version", "0..0"],
        vec!["--root", root, "--version", ""],
        vec!["--root", root, "--version", "0.50.0-rc1"],
    ] {
        assert_eq!(
            run(&args),
            (
                Some(2),
                String::new(),
                "release readme: invalid arguments\n".to_string()
            ),
            "{args:?}"
        );
    }
    assert_eq!(snapshot(dir.path()), before);
    assert!(release_readme::valid_version("0.50.0"));
    assert!(release_readme::valid_version("10.200.3000"));
    for v in [
        "0.50", "v0.50.0", "0.50.0 ", "0.5a.0", "", "0.50.0.0", "+1.2.3",
    ] {
        assert!(!release_readme::valid_version(v), "{v:?}");
    }
}

// --- 3. malformed and ambiguous sources -----------------------------------------------------------

#[test]
fn rfr4_readme_refuses_ambiguous_or_malformed_source() {
    let source = plain_source();
    let tail = format!("{VERIFY_OPEN}\n{VERIFY_INTRO}");
    let fold_msg = "release readme: source does not end with the verify fold";
    let insertion = "release readme: source has no single insertion position";
    let generated_msg = "release readme: source already holds a generated part";

    // the last two sections
    let folds_at = source.find(ABOUT_OPEN).unwrap();
    let (head, folds) = source.split_at(folds_at);
    let verify_at = folds.find(VERIFY_OPEN).unwrap();
    let (about, verify) = folds.split_at(verify_at);
    for (name, text) in [
        (
            "a section after the verify fold",
            format!("{source}\n## After\n\nWords.\n"),
        ),
        ("text after the verify fold", format!("{source}\nWords.\n")),
        (
            "the folds swapped",
            format!(
                "{head}{verify}\n{}",
                about.trim_end_matches('\n').to_string() + "\n"
            ),
        ),
        (
            "no verify fold",
            format!("{head}{}", about.trim_end_matches('\n').to_string() + "\n"),
        ),
        ("no About Release 1 fold", format!("{head}{verify}")),
        (
            "a section between the folds",
            format!("{head}{about}## Between\n\nWords.\n\n{verify}"),
        ),
        (
            "a verify fold with another title",
            source.replace("Verify the download (optional)", "Verify the download"),
        ),
        (
            "a verify fold with no summary",
            source.replace(&tail, &format!("<details>\n\n{VERIFY_INTRO}")),
        ),
        (
            "a summary in another form",
            source.replace(
                "<summary><b>Verify the download (optional)</b></summary>",
                "<summary>Verify the download (optional)</summary>",
            ),
        ),
        (
            "no blank line before the close",
            source.replace(
                "| Supported. |\n\n</details>\n",
                "| Supported. |\n</details>\n",
            ),
        ),
        (
            "the About fold nested",
            format!(
                "{head}<details>\n<summary><b>Outer</b></summary>\n\n{about}</details>\n\n{verify}"
            ),
        ),
    ] {
        refuses(&RootSpec::of(&text), 1, fold_msg);
        assert_eq!(
            release_readme::assemble(text.as_bytes(), &real_keys(), V),
            Err(ReadmeFailure::VerifyFold),
            "{name}"
        );
    }

    // one insertion position, and folds the reader can close
    for (name, text) in [
        (
            "a second verify fold inside it",
            source_of("", &format!("\n{VERIFY_OPEN}\nWords.\n\n</details>\n")),
        ),
        (
            "a second verify fold before it",
            source_of(&format!("{VERIFY_OPEN}\nWords.\n\n</details>\n\n"), ""),
        ),
        (
            "an unclosed fence swallows the close",
            source_of("", "\n```text\n"),
        ),
        (
            "an unclosed fold inside it",
            source_of(
                "",
                "\n<details>\n<summary><b>Open</b></summary>\n\nWords.\n",
            ),
        ),
        ("a stray close", source_of("</details>\n\n", "")),
    ] {
        refuses(&RootSpec::of(&text), 1, insertion);
        assert_eq!(
            release_readme::assemble(text.as_bytes(), &real_keys(), V),
            Err(ReadmeFailure::Insertion),
            "{name}"
        );
    }

    // a generated part already in place, even once
    for h in [
        "### Archives",
        "### Release signing key",
        "### Release keys",
        "### Install",
    ] {
        let text = source_of("", &format!("\n{h}\n\nOld words.\n"));
        refuses(&RootSpec::of(&text), 1, generated_msg);
        assert_eq!(
            release_readme::assemble(text.as_bytes(), &real_keys(), V),
            Err(ReadmeFailure::Generated),
            "{h}"
        );
    }
    // a whole earlier assembly
    let twice = release_readme::assemble(source.as_bytes(), &real_keys(), V).unwrap();
    refuses(&RootSpec::of(&twice), 1, generated_msg);
    // the same heading outside the verify fold, or in fenced code inside it, is authored text
    let outside = source_of(
        "### Install\n\nAuthored install words.\n\n",
        "\n```md\n### Archives\n```\n",
    );
    assert_eq!(
        release_readme::assemble(outside.as_bytes(), &real_keys(), V),
        Ok(expected(&outside, &real_keys(), V))
    );

    // UTF-8 and line ends
    let mut bad = source.clone().into_bytes();
    bad.splice(10..10, [0xff, 0xfe]);
    refuses(
        &RootSpec::bytes(bad),
        1,
        "release readme: source is not valid UTF-8",
    );
    let newline = "release readme: source must use LF line ends and end with one LF";
    for text in [
        source.replace("Intro words.\n", "Intro words.\r\n"),
        source.replace("Intro words.\n", "Intro\rwords.\n"),
        source.trim_end_matches('\n').to_string(),
        format!("{source}\n"),
        format!("{source}\n\n"),
    ] {
        refuses(&RootSpec::of(&text), 1, newline);
    }

    // the byte bounds of the source and of the assembled text
    let size = "release readme: size limit exceeded";
    let limit = release_readme::README_MAX as usize;
    assert_eq!(limit, 1_048_576);
    let pad = |n: usize| -> String {
        let base = plain_source();
        let line = "x".repeat(99) + "\n";
        let lines = (n - base.len()) / line.len();
        let rest = n - base.len() - lines * line.len();
        let mut body = line.repeat(lines);
        if rest > 0 {
            body.push_str(&"y".repeat(rest - 1));
            body.push('\n');
        }
        source_of(&format!("{body}\n"), "")
    };
    let over = pad(limit + 1 - 1);
    assert_eq!(over.len(), limit + 1);
    refuses(&RootSpec::of(&over), 1, size);
    let at = pad(limit - 1);
    assert_eq!(at.len(), limit);
    // the source fits, the assembled text does not
    refuses(&RootSpec::of(&at), 1, size);
    assert_eq!(
        release_readme::assemble(at.as_bytes(), &real_keys(), V),
        Err(ReadmeFailure::SizeLimit)
    );
    let parts = generated(&real_keys(), V).len() + 1;
    let fits = pad(limit - parts - 1);
    assert_eq!(fits.len() + parts, limit);
    assert_eq!(
        release_readme::assemble(fits.as_bytes(), &real_keys(), V).map(|s| s.len()),
        Ok(limit),
        "assembled text of exactly the bound"
    );
    let one_more = pad(limit - parts);
    assert_eq!(
        release_readme::assemble(one_more.as_bytes(), &real_keys(), V),
        Err(ReadmeFailure::SizeLimit)
    );
}

// --- 4. digest and images -------------------------------------------------------------------------

#[test]
fn rfr4_readme_checks_digest_and_every_local_image() {
    let source = image_source();
    let files = image_files();
    let assets_msg = "release readme: assets/readme does not match the source";
    let image_msg = "release readme: a local image reference is invalid";
    let ok = RootSpec::of(&source).with_files(files.clone());
    let dir = ok.build();
    writes(dir.path());
    assert_eq!(
        run_on(dir.path(), V, true),
        (Some(0), String::new(), String::new())
    );

    // the sidecar and the digest
    let digest_msg = "release readme: source digest does not match readme.sha256";
    refuses(
        &RootSpec {
            sidecar: Some(format!("{}\n", sha(b"other bytes")).into_bytes()),
            ..RootSpec::of(&source).with_files(files.clone())
        },
        1,
        digest_msg,
    );
    let mut changed = source.clone().into_bytes();
    changed[2] = b'R';
    refuses(
        &RootSpec {
            sidecar: Some(format!("{}\n", sha(source.as_bytes())).into_bytes()),
            ..RootSpec::bytes(changed).with_files(files.clone())
        },
        1,
        digest_msg,
    );
    let good = sha(source.as_bytes());
    for sidecar in [
        good.to_uppercase() + "\n",
        good.clone(),
        good.clone() + "\n\n",
        good.clone() + "\r\n",
        good[1..].to_string() + "\n",
        good.clone() + "0\n",
        format!("{good}  readme.md\n"),
        format!(" {good}\n"),
        String::new(),
        "g".repeat(64) + "\n",
    ] {
        refuses(
            &RootSpec {
                sidecar: Some(sidecar.into_bytes()),
                ..RootSpec::of(&source).with_files(files.clone())
            },
            1,
            "release readme: readme.sha256 is malformed",
        );
    }

    // the image files: missing, wrong SHA-256 name, misnamed, extra, not regular
    let [p, l, d] = [0, 1, 2].map(|i| files[i].0.clone());
    let without = |name: &str| -> Vec<(String, Vec<u8>)> {
        files.iter().filter(|(n, _)| n != name).cloned().collect()
    };
    for name in [&p, &l, &d] {
        // the plain figure, the img src and the srcset each name a file that is not there
        refuses(
            &RootSpec::of(&source).with_files(without(name)),
            1,
            image_msg,
        );
    }
    // no assets folder at all, while the source has local images
    refuses(&RootSpec::of(&source), 1, assets_msg);
    // a file whose name is not the SHA-256 of its bytes
    let wrong = png_name(b"other bytes");
    let mut renamed = without(&p);
    renamed.push((wrong.clone(), images()[0].clone()));
    refuses(
        &RootSpec::of(&source.replace(&p, &wrong)).with_files(renamed),
        1,
        assets_msg,
    );
    // misnamed files, referenced or not
    for bad in [
        "picture.png".to_string(),
        p.to_uppercase(),
        p.replace(".png", ".PNG"),
        p.replace(".png", ".exe"),
        p.replace(".png", ""),
        format!("{}.png", &p[..63]),
        format!(".{p}"),
    ] {
        let mut set = without(&p);
        set.push((bad.clone(), images()[0].clone()));
        refuses(
            &RootSpec::of(&source.replace(&p, &bad)).with_files(set.clone()),
            1,
            assets_msg,
        );
        let mut extra = files.clone();
        extra.push((bad, b"stray".to_vec()));
        refuses(&RootSpec::of(&source).with_files(extra), 1, assets_msg);
    }
    // an extra file with a valid name that the source does not use
    let mut extra = files.clone();
    let stray = picture("stray image");
    extra.push((png_name(&stray), stray));
    refuses(&RootSpec::of(&source).with_files(extra), 1, assets_msg);
    // the same bytes under two extensions: both must be used
    let mut twin = files.clone();
    twin.push((p.replace(".png", ".gif"), images()[0].clone()));
    refuses(
        &RootSpec::of(&source).with_files(twin.clone()),
        1,
        assets_msg,
    );
    let both = source.replace(
        "Badge ",
        &format!(
            "![Twin](assets/readme/{})\n\nBadge ",
            p.replace(".png", ".gif")
        ),
    );
    let dir = RootSpec::of(&both).with_files(twin).build();
    writes(dir.path());
    // a folder inside the assets folder
    let dir = ok.build();
    std::fs::create_dir(dir.path().join("assets/readme/sub")).unwrap();
    refuses_root(dir.path(), V, 1, assets_msg);
    // the assets path is a file
    let dir = RootSpec::of(&source).build();
    std::fs::create_dir_all(dir.path().join("assets")).unwrap();
    std::fs::write(dir.path().join("assets/readme"), b"not a folder").unwrap();
    refuses_root(dir.path(), V, 1, assets_msg);

    // every local reference: absolute, escaping, or naming no file there; srcset too
    for target in [
        format!("/abs/{p}"),
        format!("/assets/readme/{p}"),
        format!("C:/assets/readme/{p}"),
        format!("file:///assets/readme/{p}"),
        format!("assets/readme/../readme/{p}"),
        format!("assets/readme/./{p}"),
        format!("./assets/readme/{p}"),
        format!("../assets/readme/{p}"),
        format!("assets/other/{p}"),
        format!("assets/readme/sub/{p}"),
        format!("assets\\readme\\{p}"),
        "assets/readme/".to_string(),
        "assets/readme".to_string(),
        String::new(),
    ] {
        for (what, text) in [
            (
                "figure",
                source.replacen(&format!("(assets/readme/{p})"), &format!("({target})"), 1),
            ),
            (
                "srcset",
                source.replacen(
                    &format!("srcset=\"assets/readme/{d}\""),
                    &format!("srcset=\"{target}\""),
                    1,
                ),
            ),
            (
                "src",
                source.replacen(
                    &format!("src=\"assets/readme/{l}\""),
                    &format!("src=\"{target}\""),
                    1,
                ),
            ),
        ] {
            assert_ne!(text, source, "{what} {target}");
            let set = match what {
                "figure" => without(&p),
                "srcset" => without(&d),
                _ => without(&l),
            };
            let dir = RootSpec::of(&text).with_files(set).build();
            refuses_root(dir.path(), V, 1, image_msg);
        }
    }
    // a srcset with two candidates checks both
    let two = source.replace(
        &format!("srcset=\"assets/readme/{d}\""),
        &format!("srcset=\"assets/readme/{d} 1x, assets/readme/missing.png 2x\""),
    );
    refuses(&RootSpec::of(&two).with_files(files.clone()), 1, image_msg);
    // references in fenced code and inline code are text
    let code = source.replace(
        "Badge ",
        "```md\n![Gone](assets/readme/gone.png)\n<img src=\"/abs.png\">\n```\n\nInline `![Gone](../x.png)` code.\n\nBadge ",
    );
    let dir = RootSpec::of(&code).with_files(files.clone()).build();
    writes(dir.path());

    // the total limits, with small bounds; the release bounds are the pinned numbers
    assert_eq!(release_readme::LIMITS.text, 1_048_576);
    assert_eq!(release_readme::LIMITS.entries, 100_000);
    assert_eq!(release_readme::LIMITS.image, 67_108_864);
    assert_eq!(release_readme::LIMITS.images, 268_435_456);
    let largest = files.iter().map(|(_, b)| b.len() as u64).max().unwrap();
    let total: u64 = files.iter().map(|(_, b)| b.len() as u64).sum();
    let limited = |limits: Limits| {
        let dir = ok.build();
        let before = snapshot(dir.path());
        for mode in [Mode::Write, Mode::Check] {
            assert_eq!(
                release_readme::run_with(dir.path(), V, mode, &limits),
                Err(ReadmeFailure::SizeLimit)
            );
            assert_eq!(snapshot(dir.path()), before);
        }
    };
    let at_bounds = Limits {
        entries: 3,
        image: largest,
        images: total,
        ..release_readme::LIMITS
    };
    let dir = ok.build();
    assert_eq!(
        release_readme::run_with(dir.path(), V, Mode::Write, &at_bounds),
        Ok(())
    );
    limited(Limits {
        entries: 2,
        ..at_bounds
    });
    limited(Limits {
        image: largest - 1,
        ..at_bounds
    });
    limited(Limits {
        images: total - 1,
        ..at_bounds
    });
    limited(Limits {
        text: source.len() as u64 - 1,
        ..at_bounds
    });
    assert_eq!(
        ReadmeFailure::SizeLimit.message(),
        "release readme: size limit exceeded"
    );
}

// --- 5. offline and read-only ---------------------------------------------------------------------

#[test]
fn rfr4_readme_check_is_offline_and_read_only() {
    let keys = real_keys();
    let source = image_source();
    // a synthetic root that is no Git repository; the runner clears the environment, empties
    // PATH, points proxies at a closed port and checks the home directory stays empty
    let readme = expected(&source, &keys, V);
    let spec = RootSpec {
        readme: readme.as_bytes(),
        ..RootSpec::of(&source).with_files(image_files())
    };
    let dir = spec.build();
    assert!(!dir.path().join(".git").exists());
    let before = snapshot(dir.path());
    assert_eq!(
        run_on(dir.path(), V, true),
        (Some(0), String::new(), String::new())
    );
    assert_eq!(snapshot(dir.path()), before, "check mode writes nothing");

    // a README that differs, by one byte or entirely, or is missing
    let mismatch = "release readme: README.md does not match the assembled text";
    for other in [
        readme.replacen("rdy", "rdY", 1),
        format!("{readme}\n"),
        readme[..readme.len() - 1].to_string(),
        String::new(),
    ] {
        let dir = RootSpec {
            readme: other.as_bytes(),
            ..RootSpec::of(&source).with_files(image_files())
        }
        .build();
        let before = snapshot(dir.path());
        assert_eq!(
            run_on(dir.path(), V, true),
            (Some(1), String::new(), format!("{mismatch}\n"))
        );
        assert_eq!(snapshot(dir.path()), before);
    }
    let dir = spec.build();
    std::fs::remove_file(dir.path().join("README.md")).unwrap();
    let before = snapshot(dir.path());
    assert_eq!(
        run_on(dir.path(), V, true),
        (Some(1), String::new(), format!("{mismatch}\n"))
    );
    assert_eq!(snapshot(dir.path()), before);

    // a missing source or sidecar is a file operation failure, in both modes
    let file_msg = "release readme: file operation failed";
    for gone in ["release/readme.md", "release/readme.sha256"] {
        let dir = spec.build();
        std::fs::remove_file(dir.path().join(gone)).unwrap();
        refuses_root(dir.path(), V, 1, file_msg);
    }
    let dir = spec.build();
    std::fs::remove_file(dir.path().join("Cargo.toml")).unwrap();
    refuses_root(dir.path(), V, 1, file_msg);

    // the assembly reads local bytes only: the module and its command name no process, socket,
    // environment or home lookup
    for file in ["src/release_readme.rs", "src/bin/qa-release-readme.rs"] {
        let text =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(file)).unwrap();
        for word in [
            "Command",
            "std::net",
            "TcpStream",
            "UdpSocket",
            "env::var",
            "home_dir",
            "\"git\"",
            "git2",
            ".rdy",
        ] {
            assert!(!text.contains(word), "{file} names {word}");
        }
    }
}

// --- 6. deterministic writes ----------------------------------------------------------------------

#[test]
fn rfr4_readme_write_is_deterministic() {
    let source = image_source();
    let spec = RootSpec::of(&source).with_files(image_files());

    // two roots, and two runs on one root, write the same bytes; check mode then accepts them
    let (a, b) = (spec.build(), spec.build());
    let first = writes(a.path());
    assert_eq!(writes(b.path()), first);
    assert_eq!(writes(a.path()), first, "a second write is the same");
    assert_eq!(first, expected(&source, &real_keys(), V).into_bytes());
    for dir in [&a, &b] {
        let before = snapshot(dir.path());
        assert_eq!(
            run_on(dir.path(), V, true),
            (Some(0), String::new(), String::new())
        );
        assert_eq!(snapshot(dir.path()), before);
    }
    // the pure assembly is the same function in both modes
    assert_eq!(
        release_readme::assemble(source.as_bytes(), &real_keys(), V).unwrap(),
        expected(&source, &real_keys(), V)
    );

    // a failed generation keeps the old README: each refusal leaves every byte as it was
    let dir = spec.build();
    let old = std::fs::read(dir.path().join("README.md")).unwrap();
    std::fs::write(
        dir.path().join("release/readme.sha256"),
        format!("{}\n", sha(b"x")),
    )
    .unwrap();
    refuses_root(
        dir.path(),
        V,
        1,
        "release readme: source digest does not match readme.sha256",
    );
    assert_eq!(std::fs::read(dir.path().join("README.md")).unwrap(), old);
    let dir = spec.build();
    std::fs::remove_file(
        dir.path()
            .join(format!("assets/readme/{}", image_files()[0].0)),
    )
    .unwrap();
    refuses_root(
        dir.path(),
        V,
        1,
        "release readme: a local image reference is invalid",
    );
    assert_eq!(std::fs::read(dir.path().join("README.md")).unwrap(), old);
    // a README path that cannot be replaced: a failure, and nothing else moves
    let dir = spec.build();
    std::fs::remove_file(dir.path().join("README.md")).unwrap();
    std::fs::create_dir(dir.path().join("README.md")).unwrap();
    std::fs::write(dir.path().join("README.md/keep.txt"), b"keep\n").unwrap();
    let before = snapshot(dir.path());
    assert_eq!(
        run_on(dir.path(), V, false),
        (
            Some(1),
            String::new(),
            "release readme: file operation failed\n".to_string()
        )
    );
    assert_eq!(snapshot(dir.path()), before, "no temporary file is left");
}

// --- fix round 1: every HTML attribute form, links and non-regular inputs ------------------------

/// Every HTML spelling of an image attribute is checked: unquoted values, blanks around `=`,
/// any case, single quotes, and each `srcset` candidate. A bad target refuses in both modes and
/// changes no file; a good or remote one in any spelling passes.
#[test]
fn rfr4_readme_checks_every_html_image_attribute_form() {
    let source = image_source();
    let files = image_files();
    let image_msg = "release readme: a local image reference is invalid";
    let l = files[1].0.clone();
    let d = files[2].0.clone();
    let img = format!("<img alt=\"Paired\" src=\"assets/readme/{l}\">");
    let src =
        format!("<source media=\"(prefers-color-scheme: dark)\" srcset=\"assets/readme/{d}\">");
    assert!(source.contains(&img) && source.contains(&src));
    for bad in [
        "<img src=../private.png>",
        "<img src = \"../private.png\">",
        "<img src =../private.png alt=x>",
        "<IMG SRC=\"../x.png\">",
        "<img Src='../x.png'>",
        "<img alt=\"a\"\nsrc=\"../x.png\">",
        "<img src=/abs.png/>",
    ] {
        let text = source.replacen(
            &img,
            &format!("{bad}\n<img alt=\"Paired\" src=\"assets/readme/{l}\">"),
            1,
        );
        let dir = RootSpec::of(&text).with_files(files.clone()).build();
        refuses_root(dir.path(), V, 1, image_msg);
    }
    for bad in [
        "<source srcset = \"../private.png 2x\">".to_string(),
        "<source srcset=../private.png>".to_string(),
        "<SOURCE SRCSET='../x.png 1x'>".to_string(),
        format!("<source srcset=\"assets/readme/{d} 1x, ../x.png 2x\">"),
        format!(
            "<source srcset = 'assets/readme/{d} 1x,https://example.com/a.png 2x, /abs.png 3x'>"
        ),
    ] {
        let text = source.replacen(&src, &format!("{bad}\n{src}"), 1);
        let dir = RootSpec::of(&text).with_files(files.clone()).build();
        refuses_root(dir.path(), V, 1, image_msg);
    }
    // good local targets and remote ones, in every spelling, pass
    let good = source.replacen(
        &img,
        &format!(
            "<IMG ALT=x SRC = assets/readme/{l}>\n<img src='https://example.com/a.png'>\n<img SRC=https://example.com/b.png>\n<source SrcSet = \"assets/readme/{d} 1x, https://example.com/c.png 2x\">"
        ),
        1,
    );
    let dir = RootSpec::of(&good).with_files(files.clone()).build();
    writes(dir.path());
    assert_eq!(
        run_on(dir.path(), V, true),
        (Some(0), String::new(), String::new())
    );
}

/// Links and non-regular files: a link anywhere below the root, and a FIFO or folder in place
/// of a file, refuse at once in both modes, and nothing is read through them.
#[cfg(unix)]
#[test]
fn rfr4_readme_refuses_links_and_nonregular_inputs() {
    use rdy_qa::platform::symlink;
    let file_msg = "release readme: file operation failed";
    let assets_msg = "release readme: assets/readme does not match the source";
    let source = image_source();
    let spec = RootSpec::of(&source).with_files(image_files());
    let fifo = |p: &Path| {
        let made = Command::new("mkfifo").arg(p).status().expect("run mkfifo");
        assert!(made.success(), "mkfifo {}", p.display());
    };
    // a copy of a whole valid root outside, for the links to point at
    let outside = spec.build();
    let out = outside.path();
    let relink = |root: &Path, rel: &str, target: &Path| {
        let p = root.join(rel);
        if std::fs::symlink_metadata(&p).unwrap().is_dir() {
            std::fs::remove_dir_all(&p).unwrap();
        } else {
            std::fs::remove_file(&p).unwrap();
        }
        symlink(target, &p).unwrap();
    };
    for (rel, msg) in [
        ("release/readme.md", file_msg),
        ("release/readme.sha256", file_msg),
        ("release", file_msg),
        ("Cargo.toml", file_msg),
        ("assets/readme", assets_msg),
        ("assets", assets_msg),
    ] {
        let dir = spec.build();
        relink(dir.path(), rel, &out.join(rel));
        refuses_root(dir.path(), V, 1, msg);
    }
    // a linked image entry
    let dir = spec.build();
    let name = image_files()[0].0.clone();
    relink(
        dir.path(),
        &format!("assets/readme/{name}"),
        &out.join(format!("assets/readme/{name}")),
    );
    refuses_root(dir.path(), V, 1, assets_msg);
    // FIFOs in place of the source, the sidecar and an image: refused, never waited on
    for (rel, msg) in [
        ("release/readme.md", file_msg),
        ("release/readme.sha256", file_msg),
        (&*format!("assets/readme/{name}"), assets_msg),
    ] {
        let dir = spec.build();
        std::fs::remove_file(dir.path().join(rel)).unwrap();
        fifo(&dir.path().join(rel));
        refuses_root(dir.path(), V, 1, msg);
    }
    // a folder in place of the source
    let dir = spec.build();
    std::fs::remove_file(dir.path().join("release/readme.md")).unwrap();
    std::fs::create_dir(dir.path().join("release/readme.md")).unwrap();
    refuses_root(dir.path(), V, 1, file_msg);
    // check mode: a FIFO README is a failure, never a wait
    let dir = spec.build();
    std::fs::remove_file(dir.path().join("README.md")).unwrap();
    fifo(&dir.path().join("README.md"));
    let before = snapshot(dir.path());
    assert_eq!(
        run_on(dir.path(), V, true),
        (Some(1), String::new(), format!("{file_msg}\n"))
    );
    assert_eq!(snapshot(dir.path()), before);
}

// --- the repository's own README ----------------------------------------------------------------

/// The checked-in export makes the checked-in README: `release/readme.md` matches its sidecar
/// (64 lowercase hex and LF), every file in `assets/readme/` is named by its SHA-256, and the
/// assembly of the source under the repository's authenticated release keys at this build's
/// version is `README.md`, byte for byte. Check mode accepts the repository and writes nothing.
/// This proves reproducibility and the absence of corruption, not approval of the source.
#[test]
fn rfr4_repository_readme_matches_the_checked_export() {
    let root = repo();
    let version = env!("CARGO_PKG_VERSION");
    let source = std::fs::read(root.join("release/readme.md")).expect("release/readme.md");
    let sidecar = std::fs::read_to_string(root.join("release/readme.sha256")).expect("sidecar");
    let digest = sidecar
        .strip_suffix('\n')
        .expect("the sidecar ends with one LF");
    assert!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "the sidecar is 64 lowercase hex digits: {sidecar:?}"
    );
    assert_eq!(
        sha(&source),
        digest,
        "release/readme.md matches its sidecar"
    );

    let mut assets = 0;
    for e in std::fs::read_dir(root.join("assets/readme")).expect("assets/readme") {
        let p = e.unwrap().path();
        assert!(
            std::fs::symlink_metadata(&p).unwrap().is_file(),
            "{}",
            p.display()
        );
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        let (stem, _ext) = name.split_once('.').expect("a name with an extension");
        assert_eq!(
            stem,
            sha(&std::fs::read(&p).unwrap()),
            "{name} is named by its SHA-256"
        );
        assets += 1;
    }
    assert!(assets > 0, "the export's images are in assets/readme/");

    let readme = std::fs::read(root.join("README.md")).expect("README.md");
    let assembled = release_readme::assemble(&source, &real_keys(), version).expect("assembly");
    assert_eq!(
        assembled.as_bytes(),
        readme.as_slice(),
        "README.md is the assembly"
    );

    let before = snapshot(&root.join("assets/readme"));
    assert_eq!(release_readme::run(&root, version, Mode::Check), Ok(()));
    assert_eq!(snapshot(&root.join("assets/readme")), before);
    assert_eq!(std::fs::read(root.join("README.md")).unwrap(), readme);
}
