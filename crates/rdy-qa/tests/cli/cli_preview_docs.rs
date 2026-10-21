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

/// The trust state of a README: `Ok` when it holds exactly one well-formed state.
///
/// A signer candidate is any line that looks like a release signer, whether or not its key is
/// valid, so a placeholder or malformed line cannot hide from the pin.
fn trust_state_error(readme: &str) -> Result<(), String> {
    const HEADING: &str = "### Release signing key";
    let lines: Vec<&str> = readme.lines().collect();
    let candidate = |l: &str| {
        let t = l.trim();
        t.starts_with("rdy-release")
            || t.contains("namespaces=")
            || t.contains("ssh-ed25519")
            || t.contains("sk-ssh-ed25519")
    };
    let signers: Vec<&str> = lines.iter().copied().filter(|l| candidate(l)).collect();
    let fp_lines: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.trim().starts_with("Fingerprint:"))
        .collect();
    let headings = lines.iter().filter(|l| l.trim() == HEADING).count();
    let pending = readme.contains("Release signing key: pending");
    let ready = headings > 0 || !signers.is_empty() || !fp_lines.is_empty();
    if pending == ready {
        return Err(format!(
            "exactly one trust state: pending={pending} ready={ready}"
        ));
    }
    for dummy in ["<key-type>", "<base64>", "<SHA256-fingerprint>", "<V>"] {
        if readme.contains(dummy) {
            return Err(format!("dummy field {dummy}"));
        }
    }
    if pending {
        if !(readme.contains(&format!("\n{PENDING}\n"))
            || readme.ends_with(&format!("\n{PENDING}")))
        {
            return Err("the pending block is not exact".into());
        }
        // `ready` is false here: no signer candidate, fingerprint line or heading exists.
        return Ok(());
    }

    if headings != 1 || signers.len() != 1 || fp_lines.len() != 1 {
        return Err(format!(
            "ready state needs one heading, one signer candidate and one fingerprint: \
             {headings} / {} / {}",
            signers.len(),
            fp_lines.len()
        ));
    }
    if readme.contains("pending founder confirmation")
        || readme.contains("Preview downloads are not ready")
    {
        return Err("pending words in the ready state".into());
    }
    // heading, then the text fence holding the one signer line, then the fingerprint line
    let at = lines.iter().position(|l| l.trim() == HEADING).unwrap();
    let body: Vec<&str> = lines[at + 1..]
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .take(4)
        .collect();
    if body.len() < 4 || body[0] != "```text" || body[2] != "```" {
        return Err(format!(
            "the heading is not followed by a text fence: {body:?}"
        ));
    }
    if body[1] != signers[0] {
        return Err("the signer candidate is not the line in the fence".into());
    }
    if body[3] != fp_lines[0] {
        return Err("the fingerprint line does not follow the fence".into());
    }
    let signer_re = Regex::new(
        r#"^rdy-release namespaces="rdy-release-v1" (ssh-ed25519|sk-ssh-ed25519@openssh\.com) ([A-Za-z0-9+/]+={0,2})$"#,
    )
    .unwrap();
    let c = signer_re
        .captures(body[1])
        .ok_or_else(|| format!("malformed signer line: {}", body[1]))?;
    let fp_re = Regex::new(r"^Fingerprint: (SHA256:[A-Za-z0-9+/]{43})\.$").unwrap();
    let f = fp_re
        .captures(body[3])
        .ok_or_else(|| format!("malformed fingerprint line: {}", body[3]))?;
    let parsed = ssh_key::PublicKey::from_openssh(&format!("{} {}", &c[1], &c[2]))
        .map_err(|e| format!("the signer key does not parse: {e}"))?;
    if !parsed.comment().is_empty() {
        return Err("a comment follows the base64".into());
    }
    let fp = parsed.fingerprint(ssh_key::HashAlg::Sha256).to_string();
    if &f[1] != fp.as_str() {
        return Err(format!("the fingerprint does not match the key: {fp}"));
    }
    // the words, with the link markup removed
    let link = Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap();
    let words = link.replace_all(readme, "$1");
    if !words.contains(
        "Compare this line and fingerprint with the ones in the GitHub release notes. Use the key only when both match.",
    ) {
        return Err("the reader comparison instruction is missing".into());
    }
    let notes = Regex::new(&format!(
        r"\[GitHub release notes\]\({}/tag/v[0-9.]+\)",
        regex::escape(RELEASES)
    ))
    .unwrap();
    if !notes.is_match(readme) {
        return Err("the release notes link is missing".into());
    }
    Ok(())
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
    if let Err(e) = trust_state_error(&readme) {
        panic!("{e}");
    }
    assert!(readme.contains(&format!("[SHA256SUMS]({RELEASES}/download/v")));

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

fn ready_block(signer_block: &str, fp: &str) -> String {
    format!(
        "### Release signing key\n\n{signer_block}\n\nFingerprint: {fp}.\n\nCompare this line and fingerprint with the ones in the [GitHub release notes]({RELEASES}/tag/v0.14.0). Use the key only when both match.\n"
    )
}

/// A real key and its fingerprint, from fixed bytes.
fn sample_key() -> (String, String) {
    let pk = ssh_key::PublicKey::new(
        ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey([7u8; 32])),
        "",
    );
    let line = pk.to_openssh().unwrap();
    (line, pk.fingerprint(ssh_key::HashAlg::Sha256).to_string())
}

/// The real README with its pending block replaced by `state`.
fn readme_with(state: &str) -> String {
    let readme = read("README.md");
    assert!(
        readme.contains(PENDING),
        "the base README is in the pending state"
    );
    readme.replacen(PENDING, state, 1)
}

#[test]
fn pre2_trust_state_pin_rejects_every_bad_shape() {
    let pending_readme = read("README.md");
    assert_eq!(
        trust_state_error(&pending_readme),
        Ok(()),
        "the real README"
    );

    let (key, fp) = sample_key();
    let signer = format!("rdy-release namespaces=\"rdy-release-v1\" {key}");
    let fenced = format!("```text\n{signer}\n```");
    assert_eq!(
        trust_state_error(&readme_with(&ready_block(&fenced, &fp))),
        Ok(()),
        "the well-formed ready state passes"
    );

    let placeholder = "rdy-release namespaces=\"rdy-release-v1\" <public-key>";
    let malformed = "rdy-release namespaces=\"rdy-release-v1\" ssh-ed25519 not*base64";
    let unresolved = "rdy-release namespaces=\"rdy-release-v1\" <key-type> <base64>";
    let cases: Vec<(&str, String)> = vec![
        // pending state with any signer candidate
        (
            "pending plus a placeholder signer",
            readme_with(&format!("{PENDING}\n{placeholder}\n")),
        ),
        (
            "pending plus a malformed signer",
            readme_with(&format!("{PENDING}\n{malformed}\n")),
        ),
        (
            "pending plus a valid signer",
            readme_with(&format!("{PENDING}\n{signer}\n")),
        ),
        (
            "pending plus an indented signer",
            readme_with(&format!("{PENDING}\n    {signer}\n")),
        ),
        (
            "pending plus a fingerprint line",
            readme_with(&format!("{PENDING}\nFingerprint: {fp}.\n")),
        ),
        (
            "pending plus the ready heading",
            readme_with(&format!("{PENDING}\n### Release signing key\n")),
        ),
        // ready state
        (
            "ready with unresolved fields",
            readme_with(&ready_block(
                &format!("```text\n{unresolved}\n```"),
                "<SHA256-fingerprint>",
            )),
        ),
        (
            "ready with a malformed signer",
            readme_with(&ready_block(&format!("```text\n{malformed}\n```"), &fp)),
        ),
        (
            "ready with an extra malformed signer",
            readme_with(&ready_block(
                &format!("```text\n{signer}\n{malformed}\n```"),
                &fp,
            )),
        ),
        (
            "ready with two valid signers",
            readme_with(&ready_block(&format!("{fenced}\n\n{fenced}"), &fp)),
        ),
        (
            "ready with the signer outside any fence",
            readme_with(&ready_block(&signer, &fp)),
        ),
        (
            "ready with a fence that has no text label",
            readme_with(&ready_block(&format!("```\n{signer}\n```"), &fp)),
        ),
        (
            "ready with a signer comment",
            readme_with(&ready_block(
                &format!("```text\n{signer} me@host\n```"),
                &fp,
            )),
        ),
        (
            "ready without the heading",
            readme_with(&ready_block(&fenced, &fp).replace("### Release signing key\n\n", "")),
        ),
        (
            "ready with the signer above the heading",
            readme_with(
                &ready_block(&fenced, &fp)
                    .replace(
                        "### Release signing key\n\n",
                        &format!("{fenced}\n\n### Release signing key\n\n"),
                    )
                    .replacen(&format!("\n\n{fenced}"), "", 1),
            ),
        ),
        (
            "ready with a wrong fingerprint",
            readme_with(&ready_block(
                &fenced,
                "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            )),
        ),
        (
            "ready without the comparison sentence",
            readme_with(&ready_block(&fenced, &fp).replace(
                "Compare this line and fingerprint with the ones in the",
                "See the",
            )),
        ),
        (
            "ready and pending together",
            readme_with(&format!("{PENDING}\n{}", ready_block(&fenced, &fp))),
        ),
        ("neither state", pending_readme.replacen(PENDING, "", 1)),
    ];
    for (name, readme) in cases {
        assert!(
            trust_state_error(&readme).is_err(),
            "the pin accepted a bad README: {name}"
        );
    }
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
    for private in ["rdy-design", "rdyhub", "/Users/"] {
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
