//! `qa-release-trust` with the public receiver vectors: the original v0.35.0 manifest and root
//! signature verify for real. No machine statement here is signed: the unendorsed receiver
//! records carry a placeholder, so every machine registry fails closed at its root signature.
//! Positive cryptographic acceptance of a machine statement stays UNVERIFIED until the
//! founder-controlled rehearsal or the real endorsement supplies signed vectors.
use rdy_qa::fixtures::output_within;
use rdy_qa::release_keys::{
    self, RECORDS_FILE, ROOT_ALLOWED_SIGNERS, ROOT_FINGERPRINT, ROOT_KEY_BASE64, ROOT_KEY_TYPE,
    ReleaseKeys,
};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

/// The hang guard of one `qa-release-trust` run.
const GUARD: Duration = Duration::from_secs(120);
/// A synthetic policy commit: the tool only repeats it.
const COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";
/// The placeholder signature of an unendorsed record: it is not an SSHSIG.
const PLACEHOLDER: &[u8] = b"UNENDORSED: NOT AN SSHSIG\n";

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/release-keys/receiver-v1")
        .join(path)
}

/// Runs `qa-release-trust` with `args`.
fn trust<S: AsRef<OsStr>>(args: &[S]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qa-release-trust"));
    cmd.args(args);
    output_within(&mut cmd, GUARD).expect("run qa-release-trust")
}

/// Asserts a success with exactly `stdout`.
fn ok(out: &Output, stdout: &str) {
    assert_eq!(
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).as_ref()
        ),
        (Some(0), ""),
        "{out:?}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), stdout);
}

/// Asserts the refusal `line` with `code` and no stdout.
fn refused(out: &Output, code: i32, line: &str) {
    assert_eq!(out.status.code(), Some(code), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), format!("{line}\n"));
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// The JSON result for the historical key at `version`.
fn historical_result(version: &str, policy_commit: Option<&str>) -> String {
    let commit = policy_commit.map_or_else(|| "null".to_string(), |c| format!("\"{c}\""));
    format!(
        "{{\"fingerprint\":\"{ROOT_FINGERPRINT}\",\"policy_commit\":{commit},\"signer_line\":\"rdy-release namespaces=\\\"rdy-release-v1\\\" {ROOT_KEY_TYPE} {ROOT_KEY_BASE64}\",\"transition_sha256\":null,\"version\":\"{version}\"}}\n"
    )
}

/// A `release-keys/` directory with the three unendorsed receiver records, placeholder
/// signatures and the generated index.
fn unendorsed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in [
        "revocation-v0.40.0.json",
        "transition-v0.40.0.json",
        "transition-v0.44.0.json",
    ] {
        fs::copy(
            fixture(&format!("unendorsed-schema/{name}")),
            dir.path().join(name),
        )
        .expect("copy");
        fs::write(dir.path().join(format!("{name}.sig")), PLACEHOLDER).expect("write");
    }
    fs::copy(
        fixture("unendorsed-schema/expected-records.txt"),
        dir.path().join(RECORDS_FILE),
    )
    .expect("copy");
    dir
}

#[test]
fn rsig2_cli_selects_and_verifies_the_real_historical_release() {
    ok(
        &trust(&["select", "--version", "0.35.0"]),
        &historical_result("0.35.0", None),
    );
    let (manifest, sig) = (
        fixture("real-v0.35.0/SHA256SUMS"),
        fixture("real-v0.35.0/SHA256SUMS.sig"),
    );
    let verify = |version: &str, manifest: &Path| {
        trust(&[
            "verify".as_ref(),
            "--version".as_ref(),
            OsStr::new(version),
            "--manifest".as_ref(),
            manifest.as_os_str(),
            "--signature".as_ref(),
            sig.as_os_str(),
        ])
    };
    ok(
        &verify("0.35.0", &manifest),
        &historical_result("0.35.0", None),
    );
    let scratch = tempfile::tempdir().expect("tempdir");
    let altered = scratch.path().join("SHA256SUMS");
    let mut bytes = fs::read(&manifest).unwrap();
    bytes[0] ^= 1;
    fs::write(&altered, &bytes).unwrap();
    refused(
        &verify("0.35.0", &altered),
        1,
        "release trust: the release signature for 0.35.0 does not verify",
    );
    fs::write(&altered, vec![b'0'; 1025]).unwrap();
    refused(
        &verify("0.35.0", &altered),
        1,
        "release trust: SHA256SUMS is over 1024 bytes",
    );
    refused(
        &verify("0.40.0", &manifest),
        1,
        "release trust: no release key for 0.40.0",
    );
    refused(
        &trust(&["select", "--version", "0.36.0"]),
        1,
        "release trust: no release key for 0.36.0",
    );
    refused(
        &trust(&["select", "--version", "v0.35.0"]),
        1,
        "release trust: not a preview version",
    );
    refused(
        &trust(&["select", "--version", "0.35.0", "--signing"]),
        1,
        "release trust: the key for 0.35.0 is the historical key; it signs no new release",
    );
    // an empty registry keeps the historical key and repeats the policy commit
    let empty = tempfile::tempdir().expect("tempdir");
    fs::write(empty.path().join(RECORDS_FILE), b"").unwrap();
    let keys = empty.path().as_os_str();
    ok(
        &trust(&[
            "select".as_ref(),
            "--version".as_ref(),
            "0.20.0".as_ref(),
            "--keys".as_ref(),
            keys,
            "--policy-commit".as_ref(),
            COMMIT.as_ref(),
        ]),
        &historical_result("0.20.0", Some(COMMIT)),
    );
    // the README's historical signer line is the pinned root
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"))
        .expect("README");
    assert!(readme.contains(ROOT_ALLOWED_SIGNERS.trim_end()));
}

#[test]
fn rsig2_cli_refuses_unendorsed_records_and_bad_arguments() {
    let dir = unendorsed();
    let keys = dir.path().as_os_str();
    let first = "release trust: revocation-v0.40.0.json: the root signature does not verify";
    refused(
        &trust(&[
            "select".as_ref(),
            "--version".as_ref(),
            "0.44.0".as_ref(),
            "--keys".as_ref(),
            keys,
            "--policy-commit".as_ref(),
            COMMIT.as_ref(),
        ]),
        1,
        first,
    );
    refused(
        &trust(&["render".as_ref(), "--keys".as_ref(), keys]),
        1,
        first,
    );
    refused(
        &trust(&["versions".as_ref(), "--keys".as_ref(), keys]),
        1,
        first,
    );
    // the index generator reads names only
    ok(
        &trust(&["records".as_ref(), "--keys".as_ref(), keys]),
        &fs::read_to_string(fixture("unendorsed-schema/expected-records.txt")).unwrap(),
    );
    fs::write(dir.path().join("notes.txt"), b"x").unwrap();
    refused(
        &trust(&["records".as_ref(), "--keys".as_ref(), keys]),
        1,
        "release trust: unknown entry notes.txt",
    );

    let bad: [&[&str]; 13] = [
        &[],
        &["sign"],
        &["select"],
        &["select", "--version"],
        &["select", "--version", "0.35.0", "--keys", "/tmp"],
        &["select", "--version", "0.35.0", "--policy-commit", COMMIT],
        &[
            "select",
            "--version",
            "0.35.0",
            "--keys",
            "/tmp",
            "--policy-commit",
            "89ABCDEF0123456789abcdef0123456789abcdef",
        ],
        &[
            "select",
            "--version",
            "0.35.0",
            "--keys",
            "/tmp",
            "--policy-commit",
            "89abcdef0123456789abcdef0123456789abcde",
        ],
        &["select", "--version", "0.35.0", "--kms"],
        &["verify", "--version", "0.35.0", "--manifest", "/tmp/m"],
        &["records"],
        &["render", "--keys"],
        &["versions", "--signing"],
    ];
    for args in bad {
        refused(&trust(args), 2, "release trust: invalid arguments");
    }
    let help = trust(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("qa-release-trust"));
}

#[test]
fn rsig2_cli_renders_the_historical_trust_section_and_tables() {
    let historical = ReleaseKeys::historical();
    let section = historical.trust_section().unwrap();
    assert!(section.contains("The machine release key is pending"));
    ok(&trust(&["render"]), &section);
    let versions = historical.versions_txt();
    assert_eq!(
        release_keys::sha256_hex(versions.as_bytes()),
        "028019de9b6f9b2c9f789a291600449a97ed9eab2cf3a34c3b80e32e2cdc3f0b"
    );
    ok(&trust(&["versions"]), &versions);
    let empty = tempfile::tempdir().expect("tempdir");
    fs::write(empty.path().join(RECORDS_FILE), b"").unwrap();
    let keys = empty.path().as_os_str();
    ok(&trust(&["records".as_ref(), "--keys".as_ref(), keys]), "");
    ok(
        &trust(&["render".as_ref(), "--keys".as_ref(), keys]),
        &section,
    );
    ok(
        &trust(&["versions".as_ref(), "--keys".as_ref(), keys]),
        &versions,
    );
}
