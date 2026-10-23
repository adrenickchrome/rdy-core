//! `qa-release-sources` and the dependency policy for MPL-2.0 libraries (D-CNNKS).
use rdy_qa::fixtures::output_within;
use rdy_qa::platform;
use rdy_qa::release_notices::{MSG_INVALID_ARGUMENTS, MSG_OUTPUT_EXISTS};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

const IMBL_SHA: &str = "46bad832b9b463ed9398b8506488cc2e3b897a9d44f13af115f382aac71f4fec";
const CHUNKS_SHA: &str = "2a0813be332553f857953298749fa19549e8b61b80589757c29b4e2a804fa9c6";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Runs `qa-release-sources` with `args`.
fn run<S: AsRef<OsStr>>(args: &[S]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qa-release-sources"));
    cmd.args(args);
    output_within(&mut cmd, Duration::from_secs(600)).expect("run qa-release-sources")
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

/// The sorted names in `dir`, with each file's bytes and mode.
fn contents(dir: &Path) -> Vec<(String, Vec<u8>, u32)> {
    let mut v: Vec<(String, Vec<u8>, u32)> = fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| {
            let p = e.expect("entry").path();
            (
                p.file_name().expect("name").to_string_lossy().into_owned(),
                fs::read(&p).expect("read"),
                platform::mode_of(&p).expect("mode"),
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn plx7_source_tool_matches_the_locked_assets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("out");
    let v = env!("CARGO_PKG_VERSION");
    let done = run(&[
        "--version".as_ref(),
        OsStr::new(v),
        "--out".as_ref(),
        out.as_os_str(),
    ]);
    assert_eq!(done.status.code(), Some(0), "{done:?}");
    assert!(done.stderr.is_empty(), "{done:?}");
    let want = format!(
        concat!(
            "## Third-party source (MPL-2.0)\n",
            "\n",
            "rdy is licensed MIT OR Apache-2.0. The binaries also contain two libraries under the [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/): `imbl` 7.0.2 and `imbl-sized-chunks` 0.2.0. This release attaches the exact source code of each, as its crates.io crate file. Each SHA-256 matches rdy's `Cargo.lock` at this release:\n",
            "\n",
            "- [imbl-7.0.2.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v{v}/imbl-7.0.2.crate) — SHA-256: `{imbl}`\n",
            "- [imbl-sized-chunks-0.2.0.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v{v}/imbl-sized-chunks-0.2.0.crate) — SHA-256: `{chunks}`\n",
            "\n",
            "Each archive's `THIRD-PARTY-NOTICES.txt` gives the license of every library in the binary.\n",
        ),
        v = v,
        imbl = IMBL_SHA,
        chunks = CHUNKS_SHA,
    );
    assert_eq!(String::from_utf8_lossy(&done.stdout), want);
    let staged = contents(&out);
    let summary: Vec<(&str, String, u32)> = staged
        .iter()
        .map(|(n, b, m)| (n.as_str(), format!("{:x}", Sha256::digest(b)), *m))
        .collect();
    assert_eq!(
        summary,
        [
            ("imbl-7.0.2.crate", IMBL_SHA.to_string(), 0o644),
            (
                "imbl-sized-chunks-0.2.0.crate",
                CHUNKS_SHA.to_string(),
                0o644
            ),
        ],
        "only the two crate files, each the locked bytes"
    );

    // The same OUT again: refused, and nothing in it changes.
    let again = run(&[
        "--version".as_ref(),
        OsStr::new(v),
        "--out".as_ref(),
        out.as_os_str(),
    ]);
    refused(&again, 1, MSG_OUTPUT_EXISTS, "an existing OUT");
    assert_eq!(contents(&out), staged);

    // Arguments: exit 2, one fixed text, nothing created.
    let fresh = dir.path().join("fresh");
    let f = fresh.to_str().expect("utf-8");
    for (what, args) in [
        ("no arguments", vec![]),
        ("no --out", vec!["--version", v]),
        ("no --version", vec!["--out", f]),
        ("a bad version", vec!["--version", "0.18", "--out", f]),
        ("a relative OUT", vec!["--version", v, "--out", "out"]),
        (
            "an unknown flag",
            vec!["--version", v, "--out", f, "--sign", "x"],
        ),
        (
            "a relative notes file",
            vec!["--version", v, "--out", f, "--check-notes", "notes.md"],
        ),
    ] {
        refused(&run(&args), 2, MSG_INVALID_ARGUMENTS, what);
        assert!(fs::symlink_metadata(&fresh).is_err(), "{what}: no OUT");
    }
}

#[test]
fn plx7_dependency_policy_limits_mpl_to_two_exact_versions() {
    let text = fs::read_to_string(root().join("deny.toml")).expect("deny.toml");
    assert!(text.starts_with(
        "# Reviewed dependency policy. No advisory ignores. MPL exceptions name exact crate versions.\n"
    ));
    let policy: toml::Table = toml::from_str(&text).expect("deny.toml parses");
    let advisories = policy["advisories"].as_table().expect("advisories");
    assert_eq!(
        advisories["ignore"].as_array().map(Vec::len),
        Some(0),
        "no advisory ignore"
    );
    let licenses = policy["licenses"].as_table().expect("licenses");
    let allow: Vec<&str> = licenses["allow"]
        .as_array()
        .expect("allow")
        .iter()
        .map(|v| v.as_str().expect("a license"))
        .collect();
    assert!(
        allow.iter().all(|l| !l.contains("MPL")),
        "no global MPL allow: {allow:?}"
    );
    let exceptions = licenses["exceptions"].as_array().expect("exceptions");
    let got: Vec<(Vec<&str>, &str, Vec<&str>)> = exceptions
        .iter()
        .map(|e| {
            let t = e.as_table().expect("an exception table");
            let mut keys: Vec<&str> = t.keys().map(String::as_str).collect();
            keys.sort();
            let allow = t["allow"]
                .as_array()
                .expect("allow")
                .iter()
                .map(|v| v.as_str().expect("a license"))
                .collect();
            (keys, t["crate"].as_str().expect("crate"), allow)
        })
        .collect();
    assert_eq!(
        got,
        [
            (vec!["allow", "crate"], "imbl@7.0.2", vec!["MPL-2.0"]),
            (
                vec!["allow", "crate"],
                "imbl-sized-chunks@0.2.0",
                vec!["MPL-2.0"]
            ),
        ],
        "exactly two exact-version exceptions, with no name or version key"
    );
}
