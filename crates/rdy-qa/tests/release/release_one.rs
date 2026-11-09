//! Release 1 packaging (relc1): the binary prints the plain version, each native package has
//! its plain stem and no preview note, and the machine-read versions stay numeric. The
//! four-entry manifest and the sample hash are pinned beside the install checker, in
//! `release_install.rs` (unit tests), because they call its private manifest grammar.
use rdy_qa::fixtures::{output_within, output_within_stdin, rdy_bin, rdy_command};
use rdy_qa::platform;
use rdy_qa::release_install::{self, TARGETS, WINDOWS_TARGET};
use rdy_qa::skip::{self, Why};
use serde_json::Value;
use std::fs;
use std::process::Command;
use std::time::Duration;

const V: &str = env!("CARGO_PKG_VERSION");
const GUARD: Duration = Duration::from_secs(600);

#[test]
fn relc1_plain_version_and_three_native_packages() {
    // the built binary prints the plain version, for every spelling of the flag
    for args in [
        &["--version"][..],
        &["-V"][..],
        &["--json", "--version"][..],
    ] {
        let mut cmd = Command::new(rdy_bin());
        cmd.args(args);
        let out = output_within(&mut cmd, GUARD).expect("run rdy");
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("rdy {V}\n"),
            "{args:?}"
        );
        assert!(out.stderr.is_empty(), "{args:?}");
    }

    // the three archive names, plain and in manifest order; no ZIP and no preview mark
    let names =
        [TARGETS[0], WINDOWS_TARGET, TARGETS[1]].map(|t| release_install::archive_name(V, t));
    assert_eq!(
        names,
        [
            format!("rdy-{V}-aarch64-apple-darwin.tar.gz"),
            format!("rdy-{V}-x86_64-pc-windows-msvc.tar.gz"),
            format!("rdy-{V}-x86_64-unknown-linux-gnu.tar.gz"),
        ]
    );
    let mut sorted = names.to_vec();
    sorted.sort();
    assert_eq!(sorted, names, "name order is manifest order");

    // this machine's native package: the plain stem, the built binary, both licenses, the
    // notices and no PREVIEW.txt (the other two targets' packages are pinned by the unit tests
    // with explicit probes, because the version probe runs the binary here)
    if !release_install::tar_available() {
        skip::skip(
            &rdy_qa::test_name!("relc1_plain_version_and_three_native_packages"),
            Why::Prerequisite,
            release_install::TAR_SKIP_REASON,
        );
        return;
    }
    let Ok(target) = platform::release_target() else {
        return;
    };
    let work = tempfile::tempdir().expect("tempdir");
    let rdy = rdy_bin();
    let archive =
        release_install::package(&rdy, V, target, &work.path().join("out")).expect("package");
    let stem = format!("rdy-{V}-{target}");
    assert_eq!(
        archive.file_name().and_then(|n| n.to_str()),
        Some(format!("{stem}.tar.gz").as_str())
    );
    let into = work.path().join("x");
    fs::create_dir(&into).expect("mkdir");
    let mut cmd = Command::new(platform::tar_program());
    cmd.arg("-xzf").arg(&archive).arg("-C").arg(&into);
    let un = output_within(&mut cmd, GUARD).expect("run tar");
    assert!(un.status.success(), "{un:?}");
    let exe = if target == WINDOWS_TARGET {
        "rdy.exe"
    } else {
        "rdy"
    };
    let mut members: Vec<String> = fs::read_dir(into.join(&stem))
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    members.sort();
    let mut want = vec![
        exe.to_string(),
        release_install::MIT_FILE.to_string(),
        release_install::APACHE_FILE.to_string(),
        release_install::NOTICES_FILE.to_string(),
    ];
    want.sort();
    assert_eq!(members, want, "no PREVIEW.txt");
    assert_eq!(
        fs::read(into.join(&stem).join(exe)).expect("packaged"),
        fs::read(&rdy).expect("built"),
        "the archive holds the built binary"
    );
}

#[test]
fn relc1_release_contract_keeps_machine_versions_plain() {
    // The MCP server answers `initialize` with the numeric version, not the human text.
    let (dir, project) = rdy_qa::fixtures::project();
    drop(project);
    let mut cmd = rdy_command();
    cmd.arg("--no-daemon")
        .arg("--project")
        .arg(dir.path())
        .arg("mcp");
    let hello = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"relc1","version":"0"}}}
"#;
    let out = output_within_stdin(&mut cmd, Some(hello), GUARD).expect("run rdy mcp");
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let reply: Value = serde_json::from_str(&first).unwrap_or_else(|e| panic!("{e}: {first}"));
    assert_eq!(reply["result"]["serverInfo"]["name"], "rdy");
    assert_eq!(reply["result"]["serverInfo"]["version"], V);

    // The contract fixture changed in exactly one value: the human version text. Every machine
    // version field keeps its shape.
    let fixture: Value = serde_json::from_str(
        &fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/contracts/release-1.json"),
        )
        .expect("fixture"),
    )
    .expect("json");
    assert_eq!(
        fixture["cli"]["runtime"]["version_text"]["text"],
        "rdy <version>"
    );
    let text = serde_json::to_string(&fixture).expect("text");
    assert!(
        !text.contains("(preview)"),
        "no preview mark remains in the contract"
    );
}
