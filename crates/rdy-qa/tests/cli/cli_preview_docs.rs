//! The preview contract (PRE task 2): the human version output carries the preview mark, the
//! public README states the preview policy and one trust state, and the public tree names no
//! private project.
use rdy_qa::fixtures::{output_within, rdy_bin};
use regex::Regex;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const ABOUT: &str =
    "A system for keeping decisions and documentation consistent while agents change them.";
const RELEASES: &str = "https://github.com/adrenickchrome/rdy-core/releases";
const ISSUES: &str = "https://github.com/adrenickchrome/rdy-core/issues";
const TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"];

/// The decision's seven sentences and the report sentence, contiguous, with the heading.
const PREVIEW_NOTE: &str = "\
This is a preview of rdy.
It comes before Release 1, the first release with a compatibility promise.

Until Release 1, any command, flag, tool, code or file format may change in the next preview, with no deprecation period.
One thing stays: every later rdy, Release 1 included, reads every dump that this rdy writes.

To move a project to a newer preview, run `rdy dump --out <file>` with this rdy.
Then run `rdy load <file> <dir>` with the new rdy, into a directory with no store.
A project made with a preview moves to Release 1 the same way.

Report problems at https://github.com/adrenickchrome/rdy-core/issues.
";

const PENDING: &str = "\
Release signing key: pending founder confirmation.
Preview downloads are not ready.
Do not install a preview until this README and its GitHub release notes show the same key and fingerprint.
";

const STORE_ADVICE: &str = "\
rdy refuses a store from a different store format with E_STORE_FORMAT.
Before you replace the old binary, close its sessions and dump each project with that binary.
Stop the old project's daemon. Keep the original project and the old binary.
Use the new binary to load the dump into a directory with no store.
Check the loaded project with rdy fsck.
";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let cwd = tempfile::tempdir().unwrap();
    let out = output_within(
        Command::new(rdy_bin()).args(args).current_dir(cwd.path()),
        Duration::from_secs(60),
    )
    .unwrap_or_else(|e| panic!("rdy {args:?}: {e}"));
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn pre2_cli_version_marks_only_human_output() {
    let want = format!("rdy {} (preview)\n", env!("CARGO_PKG_VERSION"));
    for args in [
        &["--version"][..],
        &["-V"][..],
        &["--json", "--version"][..],
    ] {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, Some(0), "{args:?}: {stderr}");
        assert_eq!(stdout, want, "{args:?}");
        assert_eq!(stderr, "", "{args:?}");
    }
    let (code, stdout, stderr) = run(&["--help"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains(ABOUT), "{stdout}");
}

#[test]
fn pre2_preview_readme_has_one_trust_state() {
    let readme = read("README.md");
    let pending = readme.contains("Release signing key: pending");
    let line_re = Regex::new(
        r#"(?m)^rdy-release namespaces="rdy-release-v1" (ssh-ed25519|sk-ssh-ed25519@openssh\.com) ([A-Za-z0-9+/]+={0,2})$"#,
    )
    .unwrap();
    let lines: Vec<_> = line_re.captures_iter(&readme).collect();
    let fp_re = Regex::new(r"(?m)^Fingerprint: (SHA256:[A-Za-z0-9+/]{43})\.$").unwrap();
    let fps: Vec<_> = fp_re.captures_iter(&readme).collect();
    let ready = readme.contains("### Release signing key")
        || !lines.is_empty()
        || readme.contains("Fingerprint:");
    assert!(
        pending != ready,
        "exactly one trust state: pending={pending} ready={ready}"
    );

    // no dummy fields in either state
    for dummy in ["<key-type>", "<base64>", "<SHA256-fingerprint>", "<V>"] {
        assert!(!readme.contains(dummy), "dummy field {dummy}");
    }
    assert!(readme.contains(&format!("[SHA256SUMS]({RELEASES}/download/v")));

    if pending {
        assert!(
            readme.contains(&format!("\n{PENDING}\n")) || readme.ends_with(&format!("\n{PENDING}")),
            "the pending block is not exact"
        );
        assert!(lines.is_empty() && fps.is_empty());
        assert!(!readme.contains("### Release signing key"));
    } else {
        assert_eq!(lines.len(), 1, "one signer line");
        assert_eq!(fps.len(), 1, "one fingerprint line");
        assert!(!readme.contains("pending founder confirmation"));
        assert!(!readme.contains("Preview downloads are not ready"));
        let key = format!("{} {}", &lines[0][1], &lines[0][2]);
        let parsed = ssh_key::PublicKey::from_openssh(&key).expect("the signer key parses");
        assert!(parsed.comment().is_empty(), "no comment follows the base64");
        let fp = parsed.fingerprint(ssh_key::HashAlg::Sha256).to_string();
        assert_eq!(&fps[0][1], fp.as_str(), "the fingerprint matches the key");
        // the words, with the link markup removed
        let link = Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap();
        let words = link.replace_all(&readme, "$1");
        assert!(words.contains(
            "Compare this line and fingerprint with the ones in the GitHub release notes. Use the key only when both match."
        ));
        assert!(
            Regex::new(&format!(
                r"\[GitHub release notes\]\({}/tag/v[0-9.]+\)",
                regex::escape(RELEASES)
            ))
            .unwrap()
            .is_match(&readme)
        );
    }

    // the verification steps
    for text in [
        "Download the archive for your platform, SHA256SUMS and SHA256SUMS.sig into one new directory.",
        "Check the signature with OpenSSH, then check the archive hash, before extraction or execution. The signed file is SHA256SUMS.",
        "This signature is not Apple notarization.",
        &format!("Report problems at {ISSUES}."),
        "rdy is available under either [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your choice.",
    ] {
        assert!(readme.contains(text), "missing: {text}");
    }
    assert!(readme.contains("## License\n"));
    assert!(readme.contains("## Report problems\n"));
    let top: Vec<_> = readme.lines().take(6).collect();
    assert!(top.contains(&ABOUT), "the sentence is at the top: {top:?}");

    // every V in names and links is one plain version, not newer than this build
    let v_re = Regex::new(&format!(
        r"rdy-([^/\s)]+?)-preview-({})\.tar\.gz|releases/(?:tag|download)/v([^/\s)]+)",
        TARGETS.map(regex::escape).join("|")
    ))
    .unwrap();
    let plain = Regex::new(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$").unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for c in v_re.captures_iter(&readme) {
        seen.insert(c.get(1).or(c.get(3)).unwrap().as_str().to_string());
    }
    assert_eq!(seen.len(), 1, "one V everywhere: {seen:?}");
    let v = seen.into_iter().next().unwrap();
    let parts = plain
        .captures(&v)
        .unwrap_or_else(|| panic!("V {v} is not plain"));
    let build = plain.captures(env!("CARGO_PKG_VERSION")).unwrap();
    assert!(
        parts[1].parse::<u64>().unwrap() <= build[1].parse::<u64>().unwrap()
            && parts[2].parse::<u64>().unwrap() <= build[2].parse::<u64>().unwrap(),
        "V {v} is newer than the build"
    );
    for target in TARGETS {
        let name = format!("rdy-{v}-preview-{target}.tar.gz");
        assert!(
            readme.contains(&format!("{RELEASES}/download/v{v}/{name}")),
            "{name}"
        );
    }
    for name in ["SHA256SUMS", "SHA256SUMS.sig"] {
        assert!(
            readme.contains(&format!("{RELEASES}/download/v{v}/{name})")),
            "{name}"
        );
    }
    assert!(readme.contains(&format!("{RELEASES}/tag/v{v}")));
}

#[test]
fn pre2_preview_readme_preserves_the_dump_route() {
    let readme = read("README.md");
    assert!(
        readme.contains(&format!("## Preview archives\n\n{PREVIEW_NOTE}\n")),
        "the preview note is not exact"
    );
    let at = |s: &str| readme.find(s).unwrap_or_else(|| panic!("missing {s}"));
    assert!(at("## Preview archives") < at("## Quick start"));
    assert!(readme.contains(
        "The preview has two archives: macOS arm64 and Linux amd64. The Linux archive is qualified on Ubuntu 26."
    ));
    for target in TARGETS {
        let re = Regex::new(&format!(
            r"rdy-[0-9.]+-preview-{}\.tar\.gz",
            regex::escape(target)
        ))
        .unwrap();
        assert!(re.is_match(&readme), "{target}");
    }
    assert!(
        readme.contains("| Linux (x86_64, aarch64) |"),
        "Linux arm64 core support stays"
    );

    assert!(
        readme.contains(STORE_ADVICE),
        "the store advice is not exact"
    );
    for text in [
        "rdy dump --out <dump-file>",
        "rdy load <dump-file> <new-directory>",
        "Run the dump command in the old project. Run the load command with the new binary.",
        "`rdy_dump` and `rdy_load`",
        "git clone https://github.com/adrenickchrome/rdy-core.git",
    ] {
        assert!(readme.contains(text), "missing: {text}");
    }
    assert!(
        at("git clone https://github.com/adrenickchrome/rdy-core.git") < at("cargo install --path")
    );
    for gone in [
        "re-create it",
        "different `rdy` build",
        "delete .rdy",
        "rm -rf .rdy",
    ] {
        assert!(!readme.contains(gone), "obsolete advice: {gone}");
    }
}

#[test]
fn pre2_public_tree_names_no_private_project() {
    let agents = read("AGENTS.md");
    assert_eq!(
        agents.lines().nth(5),
        Some(
            "Design decisions and the current-state docs live in a separate rdy project, which is not public."
        ),
    );
    assert_eq!(
        agents.lines().nth(6),
        Some("Read them through rdy's MCP tools.")
    );
    assert!(!agents.contains("rdy-design"));

    let readme = read("README.md");
    for private in ["rdy-design", "rdyhub", "ncs-clarke", "docs-vc", "/Users/"] {
        assert!(!readme.contains(private), "README names {private}");
    }

    let ci = read(".github/workflows/ci.yml");
    let fast = ci
        .split("\n  fast:\n")
        .nth(1)
        .expect("the fast job")
        .split("\n  coverage:\n")
        .next()
        .unwrap();
    for env in [
        "CARGO_PROFILE_DEV_DEBUG: \"0\"",
        "CARGO_PROFILE_TEST_DEBUG: \"0\"",
        "CARGO_INCREMENTAL: \"0\"",
    ] {
        assert!(fast.contains(env), "fast job lacks {env}");
    }
    let free = fast
        .find("- name: Free disk space")
        .expect("Free disk space step");
    let steps = fast.find("    steps:\n").expect("steps");
    assert!(steps < free && free < fast.find("actions/checkout").unwrap());
    assert!(
        fast[steps..]
            .trim_start_matches("    steps:\n")
            .starts_with("      - name: Free disk space"),
        "Free disk space is the first step"
    );
    assert!(fast.contains("sudo rm -rf /usr/share/dotnet"));
    assert!(fast.contains("df -h /"));
}
