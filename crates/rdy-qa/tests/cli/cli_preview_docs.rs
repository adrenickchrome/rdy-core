//! The preview contract (PRE task 2): the human version output carries the preview mark, the
//! public README states the preview policy and one trust state, and the public tree names no
//! private project.
use rdy_qa::fixtures::output_within;
use rdy_qa::release_install::{readme_install_section, readme_ready_section};
use rdy_qa::release_keys::{
    self, ADMISSION_FILE, EVIDENCE_POLICY_FILE, HISTORICAL_FIRST, HISTORICAL_LAST, POLICY_FILE,
    RECORDS_FILE, ROOT_ALLOWED_SIGNERS, ROOT_FINGERPRINT, ROOT_KEY_BASE64, ROOT_KEY_TYPE,
    ReleaseKeys, StatementCheck, tag_of,
};
use regex::Regex;
use std::path::PathBuf;
use std::time::Duration;

const HEADLINE: &str = "Your intent, ready for anyone who needs it.";
const ABOUT: &str = "rdy stores what you intend and decided, and gives it to whoever needs it in a form they can use: a docs site for people, briefings and instructions for agents.";
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
        rdy_qa::fixtures::rdy_command()
            .args(args)
            .current_dir(cwd.path()),
        Duration::from_secs(60),
    )
    .unwrap_or_else(|e| panic!("rdy {args:?}: {e}"));
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The largest README the trust pin reads.
const README_MAX: usize = 1 << 20;
/// The most key rows in a README trust list: the historical key and 64 machine epochs.
const ROWS_MAX: usize = 65;

/// Where a line sits: outside every fence, or in a fence with this label.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fence {
    Outside,
    Sh,
    Json,
    Other,
}

/// One key row of the trust list.
#[derive(Debug)]
struct Row {
    /// The versions text as printed.
    versions: String,
    /// The first and last minor; `None` for a key with no good release.
    range: Option<(u32, Option<u32>)>,
    key_type: String,
    fp: String,
    state: String,
}

/// The one line `line` of `lines` at `*i`, then moves on.
fn expect_line(lines: &[&str], i: &mut usize, want: &str) -> Result<(), String> {
    match lines.get(*i) {
        Some(l) if *l == want => {
            *i += 1;
            Ok(())
        }
        other => Err(format!(
            "the trust list has {other:?} where {want:?} belongs"
        )),
    }
}

/// The lines of a fence that opens at `*i` with `open`, up to its close; moves past the close.
fn fence_body<'a>(lines: &[&'a str], i: &mut usize, open: &str) -> Result<Vec<&'a str>, String> {
    if lines.get(*i).copied() != Some(open) {
        return Err(format!("the trust list has no {open} fence"));
    }
    *i += 1;
    let mut body = Vec::new();
    while let Some(l) = lines.get(*i) {
        *i += 1;
        if *l == "```" {
            return Ok(body);
        }
        body.push(*l);
    }
    Err("an unclosed fence in the trust list".into())
}

/// One row of the trust table.
fn parse_row(line: &str) -> Result<Row, String> {
    let re = Regex::new(
        r"^\| (none|v0\.[1-9][0-9]{0,9}\.0 to v0\.[1-9][0-9]{0,9}\.0|v0\.[1-9][0-9]{0,9}\.0 and later) \| `([^`]+)` \| `(SHA256:[A-Za-z0-9+/]{43})` \| ([a-z][a-z0-9 .,]*) \|$",
    )
    .unwrap();
    let c = re
        .captures(line)
        .ok_or_else(|| format!("malformed key row: {line}"))?;
    let versions = c[1].to_string();
    let minor = |t: &str| release_keys::tag_minor(t).ok_or_else(|| format!("bad tag {t}"));
    let range = if versions == "none" {
        None
    } else if let Some((a, b)) = versions.split_once(" to ") {
        Some((minor(a)?, Some(minor(b)?)))
    } else {
        Some((minor(versions.trim_end_matches(" and later"))?, None))
    };
    if let Some((first, Some(last))) = range
        && last < first
    {
        return Err(format!("a range ends before it starts: {line}"));
    }
    Ok(Row {
        versions,
        range,
        key_type: c[2].to_string(),
        fp: c[3].to_string(),
        state: c[4].to_string(),
    })
}

/// A signer line's key type, base64 and fingerprint, or why it is malformed.
fn parse_signer(line: &str) -> Result<(String, String, String), String> {
    let signer_re = Regex::new(
        r#"^rdy-release namespaces="rdy-release-v1" (ssh-ed25519|sk-ssh-ed25519@openssh\.com) ([A-Za-z0-9+/]+={0,2})$"#,
    )
    .unwrap();
    let c = signer_re
        .captures(line)
        .ok_or_else(|| format!("malformed signer line: {line}"))?;
    let parsed = ssh_key::PublicKey::from_openssh(&format!("{} {}", &c[1], &c[2]))
        .map_err(|e| format!("the signer key does not parse: {e}"))?;
    if !parsed.comment().is_empty() {
        return Err("a comment follows the base64".into());
    }
    let fp = parsed.fingerprint(ssh_key::HashAlg::Sha256).to_string();
    Ok((c[1].to_string(), c[2].to_string(), fp))
}

/// The statement blocks of the trust list from `*i`: each one's exact bytes, its signature and
/// the offline root check. Returns the JSON lines of every block and its root `printf` line.
fn parse_statements<'a>(
    lines: &[&'a str],
    i: &mut usize,
    machine: &[&Row],
    key_of: &[(String, String)],
) -> Result<(Vec<&'a str>, Vec<&'a str>), String> {
    let heading =
        Regex::new(r"^#### (transition|revocation)-v0\.([1-9][0-9]{0,9})\.0\.json$").unwrap();
    let fp_re = Regex::new(r#""fingerprint": ?"(SHA256:[A-Za-z0-9+/]{43})""#).unwrap();
    let key_re = Regex::new(r#""public_key": ?"([^"]*)""#).unwrap();
    let (mut json_lines, mut roots) = (Vec::new(), Vec::new());
    let (mut transitions, mut revocations) = (0usize, 0usize);
    while lines.get(*i).copied() == Some("")
        && lines.get(*i + 1).is_some_and(|l| l.starts_with("#### "))
    {
        let name = lines[*i + 1];
        let c = heading
            .captures(name)
            .ok_or_else(|| format!("malformed statement heading: {name}"))?;
        let (what, ns) = match &c[1] {
            "transition" => ("transition", release_keys::TRANSITION_NAMESPACE),
            _ => ("revocation", release_keys::REVOCATION_NAMESPACE),
        };
        let file = &name["#### ".len()..];
        *i += 2;
        expect_line(lines, i, "")?;
        expect_line(
            lines,
            i,
            &format!(
                "The historical root key signed this {what} in namespace `{ns}`. Its exact bytes:"
            ),
        )?;
        expect_line(lines, i, "")?;
        let json = fence_body(lines, i, "```json")?;
        expect_line(lines, i, "")?;
        expect_line(lines, i, &format!("Its signature, `{file}.sig`:"))?;
        expect_line(lines, i, "")?;
        let sig = fence_body(lines, i, "```text")?;
        expect_line(lines, i, "")?;
        expect_line(lines, i, "Verify it offline with the root key:")?;
        expect_line(lines, i, "")?;
        let sh = fence_body(lines, i, "```sh")?;
        if sig.first().copied() != Some("-----BEGIN SSH SIGNATURE-----")
            || sig.last().copied() != Some("-----END SSH SIGNATURE-----")
        {
            return Err(format!("the signature of {file} is not an armored SSHSIG"));
        }
        // the statement names one machine key of the list
        let joined = json.join("\n");
        let fp = fp_re
            .captures(&joined)
            .map(|c| c[1].to_string())
            .ok_or_else(|| format!("{file} names no fingerprint"))?;
        let Some(row) = machine.iter().find(|r| r.fp == fp) else {
            return Err(format!("{file} names a key that is not in the trust list"));
        };
        if what == "transition" {
            transitions += 1;
            let want = key_of
                .iter()
                .find(|(f, _)| *f == fp)
                .map(|(_, k)| k.as_str())
                .ok_or_else(|| format!("{file} names a key with no line in the trust list"))?;
            let named = key_re.captures(&joined).map(|c| c[1].to_string());
            if named.as_deref() != Some(want) {
                return Err(format!("{file} names a different key than its fingerprint"));
            }
            let first = format!("\"{}\"", tag_of(row.range.map_or(0, |r| r.0)));
            if row.range.is_some() && !joined.contains(&first) {
                return Err(format!("{file} does not start at its row's first version"));
            }
        } else {
            revocations += 1;
        }
        // the offline check: a cd, the root allowed-signers line for this statement's namespace
        // only, and the ssh-keygen verification of the exact file
        let want_printf = format!(
            "'printf' '%s\\n' 'rdy-release namespaces=\"{ns}\" {ROOT_KEY_TYPE} {ROOT_KEY_BASE64}' > '/path/to/release-keys/root-{what}-allowed-signers' &&"
        );
        let want_verify = format!(
            "'ssh-keygen' '-Y' 'verify' '-f' 'root-{what}-allowed-signers' '-I' 'rdy-release' '-n' '{ns}' '-s' '{file}.sig' < '/path/to/release-keys/{file}'"
        );
        if sh.len() != 3
            || sh[0] != "cd '/path/to/release-keys' &&"
            || sh[1] != want_printf
            || sh[2] != want_verify
        {
            return Err(format!(
                "the root commands of {file} are not the root key's for its namespace"
            ));
        }
        json_lines.extend(json);
        roots.push(sh[1]);
    }
    if transitions != machine.len() {
        return Err(format!(
            "{transitions} transitions for {} machine keys",
            machine.len()
        ));
    }
    if revocations > machine.len() {
        return Err("more revocations than machine keys".into());
    }
    Ok((json_lines, roots))
}

/// The trust state of a README: `Ok` when it holds exactly one well-formed state.
///
/// The pending state has no release key at all. The ready state has one selected key, the key
/// of the README's version, and a trust list of every key with its versions: the historical
/// key, retired for new releases, then each machine epoch, with the machine statements and the
/// root commands that check them. A line that looks like a release signer is a candidate,
/// whether or not its key is valid, so a placeholder or malformed line cannot hide from the pin.
/// A candidate outside every `sh` fence declares a key. The selected key and each trust list row
/// declare exactly one line, and every declared line is the key of its row. A candidate inside
/// an `sh` fence uses a key: the generated install blocks pass the selected key to printf, and
/// every use is that line, byte for byte, as one quoted token. A root command for a machine
/// statement names the pinned root key in a statement namespace, and is not a release key.
fn trust_state_error(readme: &str) -> Result<(), String> {
    const HEADING: &str = "### Release signing key";
    const KEYS_HEADING: &str = "### Release keys";
    if readme.len() > README_MAX {
        return Err("the README is over the bound".into());
    }
    let lines: Vec<&str> = readme.lines().collect();
    let candidate = |l: &str| {
        let t = l.trim();
        t.starts_with("rdy-release")
            || t.contains("namespaces=")
            || t.contains("ssh-ed25519")
            || t.contains("sk-ssh-ed25519")
    };
    let mut found: Vec<(&str, Fence)> = Vec::new();
    // key rows of the trust table: parsed whole below, so they are not declarations
    let mut table_lines = 0usize;
    let (mut fenced, mut kind) = (false, Fence::Outside);
    for l in lines.iter().copied() {
        if let Some(label) = l.trim().strip_prefix("```") {
            kind = match (fenced, label) {
                (false, "sh") => Fence::Sh,
                (false, "json") => Fence::Json,
                (false, _) => Fence::Other,
                (true, _) => Fence::Outside,
            };
            fenced = !fenced;
        } else if candidate(l) {
            if kind == Fence::Outside && l.starts_with("| ") {
                table_lines += 1;
            } else {
                found.push((l, kind));
            }
        }
    }
    let fp_lines: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.trim().starts_with("Fingerprint:"))
        .collect();
    let headings = lines.iter().filter(|l| l.trim() == HEADING).count();
    let key_headings = lines.iter().filter(|l| l.trim() == KEYS_HEADING).count();
    let pending = readme.contains("Release signing key: pending");
    let ready = headings > 0 || key_headings > 0 || !found.is_empty() || !fp_lines.is_empty();
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
        // `ready` is false here: no signer candidate (declared or used), fingerprint line or
        // heading exists.
        return Ok(());
    }

    if headings != 1 || key_headings != 1 || fp_lines.len() != 1 {
        return Err(format!(
            "ready state needs one signing key heading, one trust list and one fingerprint: \
             {headings} / {key_headings} / {}",
            fp_lines.len()
        ));
    }
    if readme.contains("pending founder confirmation")
        || readme.contains("Preview downloads are not ready")
    {
        return Err("pending words in the ready state".into());
    }
    // the selected key: heading, then the text fence holding its one line, then the fingerprint
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
    let selected = body[1];
    let fp_re = Regex::new(r"^Fingerprint: (SHA256:[A-Za-z0-9+/]{43})\.$").unwrap();
    let f = fp_re
        .captures(body[3])
        .ok_or_else(|| format!("malformed fingerprint line: {}", body[3]))?;
    if body[3] != fp_lines[0] {
        return Err("the fingerprint line does not follow the fence".into());
    }
    let (_, _, selected_fp) = parse_signer(selected)?;
    if f[1] != selected_fp {
        return Err(format!(
            "the fingerprint does not match the key: {selected_fp}"
        ));
    }

    // the trust list: its table, a declared line per key with a good release, its statements
    let k = lines
        .iter()
        .position(|l| *l == KEYS_HEADING)
        .ok_or("the trust list heading")?;
    if k < at {
        return Err("the trust list comes before the selected key".into());
    }
    let mut i = k + 1;
    expect_line(&lines, &mut i, "")?;
    expect_line(
        &lines,
        &mut i,
        "Each version has exactly one release key. A signature by any other key is refused.",
    )?;
    expect_line(&lines, &mut i, "")?;
    expect_line(
        &lines,
        &mut i,
        "| Versions | Key type | Fingerprint | State |",
    )?;
    expect_line(&lines, &mut i, "| --- | --- | --- | --- |")?;
    let mut rows = Vec::new();
    while lines.get(i).is_some_and(|l| l.starts_with("| ")) {
        if rows.len() == ROWS_MAX {
            return Err("the trust list is over the bound".into());
        }
        rows.push(parse_row(lines[i])?);
        i += 1;
    }
    if table_lines != rows.len() {
        return Err("a key row outside the trust table".into());
    }
    let first = rows.first().ok_or("the trust list has no key")?;
    let history = format!(
        "{} to {}",
        tag_of(HISTORICAL_FIRST),
        tag_of(HISTORICAL_LAST)
    );
    if first.versions != history
        || first.key_type != ROOT_KEY_TYPE
        || first.fp != ROOT_FINGERPRINT
        || first.state != "historical key, retired for new releases"
    {
        return Err(format!(
            "the first key is not the historical key for {history}"
        ));
    }
    let machine: Vec<&Row> = rows[1..].iter().collect();
    // numeric order, no overlap, a finite range before every later one, one key per version
    let mut end: Option<Option<u32>> = Some(first.range.and_then(|r| r.1));
    let mut fingerprints = vec![first.fp.as_str()];
    for row in &machine {
        if fingerprints.contains(&row.fp.as_str()) {
            return Err(format!("a key serves two ranges: {}", row.fp));
        }
        fingerprints.push(&row.fp);
        if row.key_type != "ssh-ed25519" || !row.state.starts_with("machine key") {
            return Err(format!(
                "a machine row is not a machine key: {}",
                row.versions
            ));
        }
        let Some((start, last)) = row.range else {
            continue;
        };
        match end {
            Some(Some(e)) if start > e => {}
            _ => {
                return Err(format!(
                    "overlapping or unbounded ranges at {}",
                    row.versions
                ));
            }
        }
        end = Some(last);
    }
    expect_line(&lines, &mut i, "")?;
    let note = if machine.is_empty() {
        format!(
            "No other version has a release key. The machine release key is pending: no version after {} has a release key yet.",
            tag_of(HISTORICAL_LAST)
        )
    } else {
        "No other version has a release key.".to_string()
    };
    expect_line(&lines, &mut i, &note)?;
    let mut declared: Vec<&str> = Vec::new();
    let mut declared_keys: Vec<Option<String>> = Vec::new();
    let mut key_of: Vec<(String, String)> = Vec::new();
    for row in &rows {
        if row.range.is_none() {
            declared_keys.push(None);
            continue;
        }
        expect_line(&lines, &mut i, "")?;
        expect_line(
            &lines,
            &mut i,
            &format!("The allowed-signers line for {}:", row.versions),
        )?;
        expect_line(&lines, &mut i, "")?;
        let block = fence_body(&lines, &mut i, "```text")?;
        let [line] = block[..] else {
            return Err(format!(
                "the allowed-signers block of {} is not one line",
                row.versions
            ));
        };
        let (key_type, key_b64, fp) = parse_signer(line)?;
        if key_type != row.key_type || fp != row.fp {
            return Err(format!(
                "the line for {} is not the key of its row",
                row.versions
            ));
        }
        key_of.push((fp, format!("{key_type} {key_b64}")));
        declared.push(line);
        declared_keys.push(Some(line.to_string()));
    }
    if declared.first().copied() != Some(ROOT_ALLOWED_SIGNERS.trim_end()) {
        return Err("the historical line is not the pinned root".into());
    }
    let (json_lines, roots) = parse_statements(&lines, &mut i, &machine, &key_of)?;
    if lines.get(i).is_some_and(|l| !l.is_empty()) {
        return Err(format!("text after the trust list: {}", lines[i]));
    }
    // the next text is a heading: no loose words follow the trust list
    if let Some(next) = lines[i.min(lines.len())..].iter().find(|l| !l.is_empty())
        && !next.starts_with('#')
        && !next.starts_with("</details>")
    {
        return Err(format!("text after the trust list: {next}"));
    }

    // every candidate is accounted for: declarations in the selected fence and the trust list,
    // statement bytes in their blocks, root commands in their blocks, uses in install blocks
    let outside: Vec<&str> = found
        .iter()
        .filter(|(_, k)| matches!(k, Fence::Outside | Fence::Other))
        .map(|(l, _)| *l)
        .collect();
    let mut want_outside = vec![selected];
    want_outside.extend(declared.iter().copied());
    if outside != want_outside {
        return Err(format!(
            "an undeclared or extra signer candidate outside an sh fence: {outside:?}"
        ));
    }
    let in_json: Vec<&str> = found
        .iter()
        .filter(|(_, k)| *k == Fence::Json)
        .map(|(l, _)| *l)
        .collect();
    if in_json.iter().any(|l| !json_lines.contains(l)) {
        return Err("signer candidate in a json fence outside a statement".into());
    }
    // which key signs the README's version: the row that covers it
    let link = Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap();
    let words = link.replace_all(readme, "$1");
    if !words.contains(
        "Compare this line and fingerprint with the ones in the GitHub release notes. Use the key only when both match.",
    ) {
        return Err("the reader comparison instruction is missing".into());
    }
    let notes = Regex::new(&format!(
        r"\[GitHub release notes\]\({}/tag/v([0-9.]+)\)",
        regex::escape(RELEASES)
    ))
    .unwrap();
    let v = notes
        .captures(readme)
        .ok_or("the release notes link is missing")?[1]
        .to_string();
    let minor = release_keys::version_minor(&v).ok_or_else(|| format!("V {v} is not a preview"))?;
    let covering: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.range
                .is_some_and(|(a, b)| minor >= a && b.is_none_or(|b| minor <= b))
        })
        .map(|(n, _)| n)
        .collect();
    let [row] = covering[..] else {
        return Err(format!("{v} has no single key in the trust list"));
    };
    if declared_keys[row].as_deref() != Some(selected) {
        return Err(format!("the selected key is not the key of {v}"));
    }
    // each install use, and each root command, is accounted for
    let quoted = format!("'{selected}'");
    let mut root_found = Vec::new();
    for (l, k) in &found {
        if *k != Fence::Sh {
            continue;
        }
        if l.contains("namespaces=\"rdy-release-signer-") {
            root_found.push(*l);
        } else if !l.contains(&quoted) || candidate(&l.replace(&quoted, "")) {
            return Err(format!("an install block uses another signer line: {l}"));
        }
    }
    let mut want_roots = roots.clone();
    want_roots.sort_unstable();
    root_found.sort_unstable();
    if root_found != want_roots {
        return Err("a root command outside its statement block".into());
    }
    let notes_link = Regex::new(&format!(
        r"\[GitHub release notes\]\({}/tag/v[0-9.]+\)",
        regex::escape(RELEASES)
    ))
    .unwrap();
    if !notes_link.is_match(readme) {
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

/// A version the historical key signed: the synthetic READMEs below use it.
const RV: &str = "0.20.0";

/// The release notes link of `v`.
fn notes_url(v: &str) -> String {
    format!("{RELEASES}/tag/v{v}")
}

/// The historical key's allowed-signers line, its type with base64, and its fingerprint.
fn root_key() -> (String, String, String) {
    (
        ROOT_ALLOWED_SIGNERS.trim_end().to_string(),
        format!("{ROOT_KEY_TYPE} {ROOT_KEY_BASE64}"),
        ROOT_FINGERPRINT.to_string(),
    )
}

/// The selected-key block of the ready state for `signer_block` and `fp`, then the trust list of
/// the historical key alone. The release notes link names version `RV`.
fn ready_block(signer_block: &str, fp: &str) -> String {
    format!(
        "### Release signing key\n\n{signer_block}\n\nFingerprint: {fp}.\n\nCompare this line and fingerprint with the ones in the [GitHub release notes]({}). Use the key only when both match.\n\n{}",
        notes_url(RV),
        ReleaseKeys::historical()
            .trust_section()
            .expect("trust list"),
    )
}

/// A synthetic README in the pending state. It does not depend on the real README, so the pin
/// tests keep working when the real README moves between states.
fn pending_fixture() -> String {
    format!("# Synthetic README\n\nSome text before.\n\n{PENDING}\n## After\n\nSome text after.\n")
}

/// The synthetic README with its pending block replaced by `state`.
fn readme_with(state: &str) -> String {
    pending_fixture().replacen(PENDING, state, 1)
}

/// The README's generated install section (PRE task 3) for `signer`: its `sh` fences use the
/// signer line in their printf commands.
fn install_section(signer: &str) -> String {
    readme_install_section(RV, signer).expect("install section")
}

/// The synthetic ready README with `trust` as its trust state, then the generated install
/// section for `signer`.
fn ready_with_install(trust: &str, signer: &str) -> String {
    format!("{}\n{}", readme_with(trust), install_section(signer))
}

/// A second disposable key's signer line, from other fixed bytes.
fn other_signer() -> String {
    signer_of(8)
}

/// The release signer line of the disposable Ed25519 key made of `byte` repeated.
fn signer_of(byte: u8) -> String {
    let pk = ssh_key::PublicKey::new(
        ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey([byte; 32])),
        "",
    );
    format!(
        "rdy-release namespaces=\"rdy-release-v1\" {}",
        pk.to_openssh().unwrap()
    )
}

/// STRUCTURAL MOCK: accepts every record without any cryptography. The README tests check the
/// README's shape against records, never a signature.
struct Structural;

impl StatementCheck for Structural {
    fn check(&self, _: &str, _: &[u8], _: &[u8]) -> bool {
        true
    }
}

/// The armored shape of a signature. It is not a signature: only `Structural` accepts it.
const ARMOR: &[u8] =
    b"-----BEGIN SSH SIGNATURE-----\nU1NIU0lHAAAAAQ==\n-----END SSH SIGNATURE-----\n";

/// A synthetic evidence policy: its top level only, with placeholder nested values.
const EVIDENCE: &[u8] = b"{\"approval_channel\":null,\"coordinators\":[],\"evidence_workflow\":{},\"release_workflow_path\":\".github/workflows/release.yml\",\"repository_id\":1403751388,\"schema\":1}\n";

/// The trust records of synthetic machine epochs, each `(first, last good, key byte)`: a
/// transition per epoch, a revocation after each epoch that has a last good version, and a
/// policy for the last epoch. The epochs are disposable keys, never release keys.
fn machine_keys(epochs: &[(u32, Option<u32>, u8)]) -> ReleaseKeys {
    let dir = tempfile::tempdir().expect("tempdir");
    write_machine_records(dir.path(), epochs);
    ReleaseKeys::load_checked_by(dir.path(), &Structural).expect("synthetic machine records")
}

/// Writes the records, the index and the two policies of [`machine_keys`] into `dir`.
fn write_machine_records(dir: &std::path::Path, epochs: &[(u32, Option<u32>, u8)]) {
    let mut names = Vec::new();
    let write = |name: &str, bytes: &[u8]| std::fs::write(dir.join(name), bytes).unwrap();
    let mut last_fp = String::new();
    for (first, last, byte) in epochs {
        let pk = ssh_key::PublicKey::new(
            ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey([*byte; 32])),
            "",
        );
        let key = pk.to_openssh().unwrap();
        let fp = pk.fingerprint(ssh_key::HashAlg::Sha256).to_string();
        let t = format!("transition-v0.{first}.0.json");
        write(
            &t,
            format!("{{\n  \"fingerprint\": \"{fp}\",\n  \"first_machine_tag\": \"v0.{first}.0\",\n  \"public_key\": \"{key}\",\n  \"repository\": \"adrenickchrome/rdy-core\",\n  \"scheme\": \"rdy-release-signer-transition-v1\"\n}}\n").as_bytes(),
        );
        write(&format!("{t}.sig"), ARMOR);
        names.push(t);
        if let Some(last) = last {
            let r = format!("revocation-v0.{first}.0.json");
            write(
                &r,
                format!("{{\n  \"fingerprint\": \"{fp}\",\n  \"first_machine_tag\": \"v0.{first}.0\",\n  \"last_good_tag\": \"v0.{last}.0\",\n  \"repository\": \"adrenickchrome/rdy-core\",\n  \"scheme\": \"rdy-release-signer-revocation-v1\"\n}}\n").as_bytes(),
            );
            write(&format!("{r}.sig"), ARMOR);
            names.push(r);
        }
        last_fp = fp;
    }
    let records = release_keys::records_index(names.iter().map(String::as_str));
    write(RECORDS_FILE, records.as_bytes());
    write(EVIDENCE_POLICY_FILE, EVIDENCE);
    let first = epochs.last().expect("an epoch").0;
    write(
        POLICY_FILE,
        format!(
            "{{\"evidence_policy_sha256\":\"{}\",\"fingerprint\":\"{last_fp}\",\"first_machine_tag\":\"v0.{first}.0\",\"key_arn\":\"arn:aws:kms:us-east-1:000000000000:key/00000000-0000-0000-0000-000000000000\",\"records_sha256\":\"{}\",\"repository\":\"adrenickchrome/rdy-core\",\"role_arn\":\"arn:aws:iam::000000000000:role/synthetic-test-role\",\"schema\":1,\"signer_workflow_sha\":\"0000000000000000000000000000000000000000\",\"targets\":[\"aarch64-apple-darwin\",\"x86_64-unknown-linux-gnu\"]}}\n",
            release_keys::sha256_hex(EVIDENCE),
            release_keys::sha256_hex(records.as_bytes()),
        )
        .as_bytes(),
    );
}

/// Two machine epochs: key 7 from v0.50.0 through v0.55.0, then key 8 from v0.60.0.
fn two_epochs() -> ReleaseKeys {
    machine_keys(&[(50, Some(55), 7), (60, None, 8)])
}

/// The synthetic ready README for `v`, generated from `keys`.
fn ready_readme(keys: &ReleaseKeys, v: &str) -> String {
    readme_with(&readme_ready_section(keys, v, &notes_url(v)).expect("ready section"))
}

/// The real README must pass in the pending state and in the ready state. The other state is
/// built from the real text, so a change in either state cannot fail the pin tests for the
/// wrong reason.
#[test]
fn pre2_trust_state_pin_accepts_the_real_readme_in_either_state() {
    let real = read("README.md");
    assert_eq!(trust_state_error(&real), Ok(()), "the real README");
    let v = Regex::new(r"releases/download/v([0-9.]+)/")
        .unwrap()
        .captures(&real)
        .expect("a V")[1]
        .to_string();
    if real.contains(PENDING) {
        let ready = real.replacen(
            PENDING,
            &readme_ready_section(&ReleaseKeys::historical(), &v, &notes_url(&v)).unwrap(),
            1,
        );
        assert_eq!(
            trust_state_error(&ready),
            Ok(()),
            "the real README in the ready state with its trust list and install blocks"
        );
    } else {
        // the pending state from the real text: everything from the selected key to the end
        // of the verification details becomes the pending block
        let start = real
            .find("### Release signing key\n")
            .expect("the key heading");
        let end = real.rfind("</details>").expect("the details end");
        let pending = format!("{}{PENDING}\n{}", &real[..start], &real[end..]);
        assert_eq!(
            trust_state_error(&pending),
            Ok(()),
            "the real README in the pending state"
        );
    }
}

/// The ready state in each epoch's version: one selected key per version, never one key in the
/// whole document. The trust list holds every epoch, the statements and the root commands.
#[test]
fn rsig3_readme_lists_epochs_and_uses_the_selected_key() {
    let historical = ReleaseKeys::historical();
    let two = two_epochs();
    let (root_line, _, root_fp) = root_key();
    let key7 = signer_of(7);
    let key8 = signer_of(8);

    // the historical key alone: the honest pending machine state
    for v in ["0.17.0", RV, "0.35.0"] {
        let readme = ready_readme(&historical, v);
        assert_eq!(trust_state_error(&readme), Ok(()), "{v}");
        assert!(readme.contains(&format!("\n{root_line}\n```\n\nFingerprint: {root_fp}.\n")));
        assert!(
            readme.contains("The machine release key is pending: no version after"),
            "{v}"
        );
        assert_eq!(readme.matches("Fingerprint: ").count(), 1);
    }

    // two machine epochs: each version selects its own epoch's key, the trust list is the same
    let lists = |readme: &str| -> String {
        let at = readme.find("### Release keys\n").expect("trust list");
        let end = readme.find("### Install\n").expect("install");
        readme[at..end].to_string()
    };
    let mut list = None;
    for (v, want) in [
        ("0.17.0", &root_line),
        ("0.35.0", &root_line),
        ("0.50.0", &key7),
        ("0.55.0", &key7),
        ("0.60.0", &key8),
        ("0.99.0", &key8),
    ] {
        let readme = ready_readme(&two, v);
        assert_eq!(trust_state_error(&readme), Ok(()), "{v}");
        let selected = readme
            .split("### Release signing key\n\n```text\n")
            .nth(1)
            .and_then(|r| r.lines().next())
            .expect("the selected key");
        assert_eq!(selected, want, "{v}");
        // the install blocks use the selected key only
        for line in [&root_line, &key7, &key8] {
            let uses = readme.matches(&format!("'{line}'")).count();
            assert_eq!(uses, if line == want { 3 } else { 0 }, "{v}: {line}");
        }
        let this = lists(&readme);
        assert_eq!(*list.get_or_insert_with(|| this.clone()), this, "{v}");
    }
    let list = list.unwrap();
    for row in [
        format!(
            "| v0.17.0 to {} | `{ROOT_KEY_TYPE}` | `{ROOT_FINGERPRINT}` |",
            tag_of(HISTORICAL_LAST)
        ),
        "| v0.50.0 to v0.55.0 | `ssh-ed25519` |".to_string(),
        "| v0.60.0 and later | `ssh-ed25519` |".to_string(),
        "#### transition-v0.50.0.json".to_string(),
        "#### revocation-v0.50.0.json".to_string(),
        "#### transition-v0.60.0.json".to_string(),
    ] {
        assert!(list.contains(&row), "{row}\n{list}");
    }
    assert!(
        !list.contains("is pending"),
        "a machine epoch ends the pending state"
    );

    // versions in a gap or outside every range have no README at all
    for v in ["0.16.0", "0.56.0", "0.59.0", "0.60.1"] {
        assert!(
            readme_ready_section(&two, v, &notes_url(v)).is_err(),
            "{v} has no key"
        );
    }
    // the selected key must be the key of the README's version
    let wrong =
        ready_readme(&two, "0.60.0").replacen(&notes_url("0.60.0"), &notes_url("0.50.0"), 1);
    assert!(trust_state_error(&wrong).is_err());
    let wrong =
        ready_readme(&two, "0.35.0").replacen(&notes_url("0.35.0"), &notes_url("0.50.0"), 1);
    assert!(trust_state_error(&wrong).is_err());

    // the real README: the trust list is the one the authenticated records generate
    let real = read("README.md");
    let keys = ReleaseKeys::for_repository(&root()).expect("the repository's trust");
    assert_eq!(real_trust_list_problem(&real, &keys), Ok(()));

    // a cutover README, generated from machine records, passes the same comparison; the
    // historical-only list does not match it, and the cutover list does not match a README
    // that still says the machine key is pending
    let cutover = ready_readme(&two, "0.60.0");
    assert_eq!(real_trust_list_problem(&cutover, &two), Ok(()));
    assert!(real_trust_list_problem(&cutover, &historical).is_err());
    assert!(real_trust_list_problem(&ready_readme(&historical, RV), &two).is_err());
}

/// The YubiKey range runs through v0.41.0 (D-Z5ES3), and the real README says so.
#[test]
fn rsig3_historical_range_runs_through_v0_41_0() {
    assert_eq!(HISTORICAL_LAST, 41);
    let real = read("README.md");
    assert!(
        real.contains("| v0.17.0 to v0.41.0 | `sk-ssh-ed25519@openssh.com` |"),
        "the trust row"
    );
    assert!(real.contains("The allowed-signers line for v0.17.0 to v0.41.0:"));
    assert!(!real.contains("v0.17.0 to v0.35.0"));
    assert!(!real.contains("no version after v0.35.0"));
    let keys = ReleaseKeys::historical();
    for v in ["0.35.0", "0.36.0", "0.41.0"] {
        assert!(keys.signer_for(v).is_ok(), "{v}");
    }
    assert!(keys.signer_for("0.42.0").is_err());
}

/// A complete enrolled `release-keys/` directory, every support and control file included, goes
/// through the loader the real README tests use. The fixture is the one the Python controller
/// test parses, so the two readers agree on the file's name and fields.
#[test]
fn rsig3_complete_enrolled_directory_reaches_the_readme_checks() {
    let root_dir = tempfile::tempdir().expect("tempdir");
    let dir = root_dir.path().join("release-keys");
    std::fs::create_dir(&dir).unwrap();
    let epochs = [(50, Some(55), 7), (60, None, 8)];
    write_machine_records(&dir, &epochs);
    let versions = ReleaseKeys::for_repository_checked_by(root_dir.path(), &Structural)
        .expect("the records")
        .versions_txt();
    std::fs::write(dir.join(release_keys::VERSIONS_FILE), versions).unwrap();
    std::fs::write(
        dir.join(release_keys::README_FILE),
        "Public release keys.\n",
    )
    .unwrap();
    std::fs::write(dir.join(release_keys::ROOT_FILE), ROOT_ALLOWED_SIGNERS).unwrap();
    std::fs::copy(
        root()
            .join("crates/rdy-qa/fixtures/release-keys/control")
            .join(ADMISSION_FILE),
        dir.join(ADMISSION_FILE),
    )
    .expect("the admission fixture");

    let keys = ReleaseKeys::for_repository_checked_by(root_dir.path(), &Structural)
        .expect("the complete enrolled directory");
    assert_eq!(keys, two_epochs());
    let cutover = ready_readme(&keys, "0.60.0");
    assert_eq!(trust_state_error(&cutover), Ok(()));
    assert_eq!(real_trust_list_problem(&cutover, &keys), Ok(()));

    // the real check: the signed records still fail, on the signature, not on the listing
    let refused = ReleaseKeys::for_repository(root_dir.path()).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "release trust: revocation-v0.50.0.json: the root signature does not verify"
    );

    // one more unknown entry still refuses the whole directory
    std::fs::write(dir.join("notes.txt"), "x").unwrap();
    assert_eq!(
        ReleaseKeys::for_repository_checked_by(root_dir.path(), &Structural)
            .unwrap_err()
            .to_string(),
        "release trust: unknown entry notes.txt"
    );
}

/// The signed release-key fixtures are pinned by the hash of their bytes. No file under
/// `fixtures/release-keys` may be text-normalised (a Windows checkout with core.autocrlf).
#[test]
fn rsig3_release_key_fixtures_are_not_text_normalised() {
    let attributes = read(".gitattributes");
    assert!(
        attributes
            .lines()
            .any(|l| l.split_whitespace().collect::<Vec<_>>()
                == ["crates/rdy-qa/fixtures/release-keys/**", "-text"]),
        "the release-key fixtures need a `-text` rule in .gitattributes"
    );
    let base = root().join("crates/rdy-qa/fixtures/release-keys");
    let mut files = Vec::new();
    let mut dirs = vec![base];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    assert!(!files.is_empty(), "no release-key fixtures found");
    for file in files {
        let rel = file.strip_prefix(root()).unwrap().to_owned();
        let out = std::process::Command::new("git")
            .args(["check-attr", "text", "--"])
            .arg(&rel)
            .current_dir(root())
            .output();
        let Ok(out) = out else { continue };
        if !out.status.success() {
            continue;
        }
        let line = String::from_utf8_lossy(&out.stdout);
        assert!(
            line.trim_end().ends_with("text: unset"),
            "{} is text-normalised: {line}",
            rel.display()
        );
    }
}

/// The real README holds the trust list that `keys` (the authenticated records) generate. A
/// README still in the pending key state holds none.
fn real_trust_list_problem(real: &str, keys: &ReleaseKeys) -> Result<(), String> {
    if real.contains(PENDING) {
        return Ok(());
    }
    let trust = keys.trust_section().map_err(|e| e.to_string())?;
    if real.contains(&trust) {
        Ok(())
    } else {
        Err("the README does not hold the generated trust list".into())
    }
}

/// A root command checks a machine statement against the pinned root in a statement namespace.
/// It looks like a release signer line, but it is not a release key: it is neither a
/// declaration nor an install use.
#[test]
fn rsig3_root_commands_are_not_release_key_candidates() {
    let two = two_epochs();
    let readme = ready_readme(&two, "0.60.0");
    let commands: Vec<&str> = readme
        .lines()
        .filter(|l| l.contains("namespaces=\"rdy-release-signer-"))
        .collect();
    assert_eq!(
        commands.len(),
        3,
        "a root command for each of three statements: {commands:?}"
    );
    for c in &commands {
        assert!(c.contains(ROOT_KEY_BASE64), "{c}");
        assert!(!c.contains("rdy-release-v1"), "{c}");
    }
    assert_eq!(trust_state_error(&readme), Ok(()));
    // the same commands with no machine epoch: none are rendered, and the pin has nothing to skip
    let historical = ready_readme(&ReleaseKeys::historical(), RV);
    assert!(!historical.contains("namespaces=\"rdy-release-signer-"));
    assert_eq!(trust_state_error(&historical), Ok(()));

    let one = commands[0];
    let bad: Vec<(&str, String)> = vec![
        (
            "a root command in the manifest namespace is an install use of another line",
            readme.replacen(
                one,
                &one.replacen("rdy-release-signer-transition-v1", "rdy-release-v1", 1),
                1,
            ),
        ),
        (
            "a root command for the revocation namespace in a transition block",
            readme.replacen(
                one,
                &one.replacen(
                    "rdy-release-signer-transition-v1",
                    "rdy-release-signer-revocation-v1",
                    1,
                ),
                1,
            ),
        ),
        (
            "a root command naming a key other than the pinned root",
            readme.replacen(
                one,
                &one.replacen(
                    ROOT_KEY_BASE64,
                    &signer_of(9)[signer_of(9).rfind(' ').unwrap() + 1..],
                    1,
                ),
                1,
            ),
        ),
        (
            "a root command copied into an install block",
            readme.replacen(
                "### Install\n\n",
                &format!("### Install\n\n```sh\n{one}\n```\n\n"),
                1,
            ),
        ),
        (
            "a root command with a release key in its statement namespace",
            readme.replacen(one, &format!("{one}\n{}", signer_of(7)), 1),
        ),
        (
            "a statement block without its root command",
            readme.replacen(&format!("{one}\n"), "", 1),
        ),
        (
            "a root command with an unknown statement namespace",
            readme.replacen(
                one,
                &one.replacen(
                    "rdy-release-signer-transition-v1",
                    "rdy-release-signer-other-v1",
                    1,
                ),
                1,
            ),
        ),
    ];
    for (name, readme) in bad {
        assert!(trust_state_error(&readme).is_err(), "accepted: {name}");
    }
}

/// A key from another epoch, a test root and every reuse of a key are refused.
#[test]
fn rsig3_wrong_epoch_or_test_root_is_rejected() {
    let two = two_epochs();
    let (root_line, root_type_b64, root_fp) = root_key();
    let key7 = signer_of(7);
    let key8 = signer_of(8);
    let test_root = signer_of(9);
    let test_fp = {
        let b64 = test_root.rsplit(' ').next().unwrap();
        ssh_key::PublicKey::from_openssh(&format!("ssh-ed25519 {b64}"))
            .unwrap()
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string()
    };
    let test_b64 = test_root.rsplit(' ').next().unwrap().to_string();

    let v60 = ready_readme(&two, "0.60.0");
    let v35 = ready_readme(&two, "0.35.0");
    let v50 = ready_readme(&two, "0.50.0");
    assert_eq!(trust_state_error(&v60), Ok(()));
    let fence = |l: &str| format!("```text\n{l}\n```\n\nFingerprint:");
    let swap_selected =
        |readme: &str, from: &str, to: &str| readme.replacen(&fence(from), &fence(to), 1);
    let fp_of = |signer: &str| {
        let b64 = signer.rsplit(' ').next().unwrap();
        ssh_key::PublicKey::from_openssh(&format!("ssh-ed25519 {b64}"))
            .unwrap()
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string()
    };

    let mut cases: Vec<(&str, String)> = Vec::new();
    // a wrong epoch's key as the selected key, with its own fingerprint
    cases.push((
        "v0.60.0 selecting the earlier machine epoch's key",
        swap_selected(&v60, &key8, &key7).replacen(
            &format!("Fingerprint: {}.", fp_of(&key8)),
            &format!("Fingerprint: {}.", fp_of(&key7)),
            1,
        ),
    ));
    cases.push((
        "v0.35.0 selecting a machine key",
        swap_selected(&v35, &root_line, &key7).replacen(
            &format!("Fingerprint: {root_fp}."),
            &format!("Fingerprint: {}.", fp_of(&key7)),
            1,
        ),
    ));
    cases.push((
        "v0.50.0 selecting the historical key",
        swap_selected(&v50, &key7, &root_line).replacen(
            &format!("Fingerprint: {}.", fp_of(&key7)),
            &format!("Fingerprint: {root_fp}."),
            1,
        ),
    ));
    // install blocks that use another epoch's key
    cases.push(("v0.60.0 install blocks for the earlier epoch", {
        let ours = install_section_for(&v60);
        v60.replacen(&ours, &readme_install_section("0.60.0", &key7).unwrap(), 1)
    }));
    cases.push(("v0.35.0 install blocks for a machine key", {
        let ours = install_section_for(&v35);
        v35.replacen(&ours, &readme_install_section("0.35.0", &key8).unwrap(), 1)
    }));
    // a test root in the trust list or as the root of a statement
    cases.push((
        "the historical row replaced by a test root",
        v60.replace(ROOT_KEY_BASE64, &test_b64)
            .replace(&root_fp, &test_fp),
    ));
    cases.push((
        "a test root as the root of every statement command",
        v60.replace(
            &format!("{ROOT_KEY_TYPE} {ROOT_KEY_BASE64}' >"),
            &format!("ssh-ed25519 {test_b64}' >"),
        ),
    ));
    cases.push((
        "a test root added as a second historical key",
        v60.replacen(
            "### Release keys\n",
            &format!("### Release keys\n\n{test_root}\n"),
            1,
        ),
    ));
    // the historical range changed or reused
    cases.push((
        "the historical range cut short",
        v60.replace(
            &format!("v0.17.0 to {}", tag_of(HISTORICAL_LAST)),
            &format!("v0.17.0 to {}", tag_of(HISTORICAL_LAST - 1)),
        ),
    ));
    cases.push((
        "the first machine epoch overlapping the historical range",
        v60.replace(
            "v0.50.0 to v0.55.0",
            &format!("{} to v0.55.0", tag_of(HISTORICAL_LAST)),
        ),
    ));
    cases.push((
        "a machine epoch before the historical range ends",
        v60.replace(
            "v0.50.0 to v0.55.0",
            &format!("{} to v0.55.0", tag_of(HISTORICAL_FIRST)),
        ),
    ));
    cases.push((
        "an open-ended epoch before a later one",
        v60.replace("v0.50.0 to v0.55.0", "v0.50.0 and later"),
    ));
    cases.push((
        "one key in two ranges",
        v60.replace(&fp_of(&key8), &fp_of(&key7)),
    ));
    cases.push((
        "a machine key that reuses the historical key",
        v60.replace(&fp_of(&key7), &root_fp),
    ));
    cases.push((
        "a machine row with the historical key type",
        v60.replacen("| `ssh-ed25519` |", &format!("| `{ROOT_KEY_TYPE}` |"), 1),
    ));
    let _ = root_type_b64;
    for (name, readme) in cases {
        assert!(trust_state_error(&readme).is_err(), "accepted: {name}");
    }
}

/// The install section of a generated ready README.
fn install_section_for(readme: &str) -> String {
    let at = readme.find("### Install\n").expect("install blocks");
    let end = readme
        .find("\n## After\n")
        .expect("the fixture's next section");
    format!("{}\n", readme[at..end].trim_end_matches('\n'))
}

#[test]
fn pre2_trust_state_pin_rejects_every_bad_shape() {
    let pending_readme = pending_fixture();
    assert_eq!(
        trust_state_error(&pending_readme),
        Ok(()),
        "the synthetic pending state passes"
    );

    let (signer, key, fp) = root_key();
    let fenced = format!("```text\n{signer}\n```");
    assert_eq!(
        trust_state_error(&readme_with(&ready_block(&fenced, &fp))),
        Ok(()),
        "the well-formed ready state passes"
    );
    // The README declares each key once, in the selected block and the trust list. The
    // generated install blocks use the selected key in their printf commands, byte for byte:
    // those uses are not declarations.
    let combined = ready_with_install(&ready_block(&fenced, &fp), &signer);
    assert_eq!(
        combined.matches(signer.as_str()).count(),
        5,
        "two declarations and three uses"
    );
    assert_eq!(
        trust_state_error(&combined),
        Ok(()),
        "the ready state with its generated install blocks passes"
    );

    let placeholder = "rdy-release namespaces=\"rdy-release-v1\" <public-key>";
    let malformed = "rdy-release namespaces=\"rdy-release-v1\" ssh-ed25519 not*base64";
    let unresolved = "rdy-release namespaces=\"rdy-release-v1\" <key-type> <base64>";
    let two = two_epochs();
    let machine = ready_readme(&two, "0.60.0");
    let trust = ReleaseKeys::historical().trust_section().unwrap();
    let rows_at = trust.find("| v0.17.0").unwrap();
    let row = trust[rows_at..].lines().next().unwrap().to_string();
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
        (
            "pending plus the trust list heading",
            readme_with(&format!("{PENDING}\n### Release keys\n")),
        ),
        (
            "pending plus a machine root command",
            readme_with(&format!(
                "{PENDING}\n'printf' '%s\\n' 'rdy-release namespaces=\"rdy-release-signer-transition-v1\" {key}'\n"
            )),
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
            "ready with two valid signers in the selected block",
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
        // the trust list
        (
            "ready without the trust list",
            readme_with(&ready_block(&fenced, &fp).replace(&trust, "")),
        ),
        (
            "ready with the trust list twice",
            readme_with(&format!("{}\n{trust}", ready_block(&fenced, &fp))),
        ),
        (
            "ready with a trust list before the selected key",
            readme_with(&format!("{trust}\n{}", ready_block(&fenced, &fp).replace(&trust, ""))),
        ),
        (
            "ready with a malformed key row",
            readme_with(&ready_block(&fenced, &fp).replace(&row, &row.replace("| `sk-", "| sk-"))),
        ),
        (
            "ready with a key row for a gap range",
            readme_with(&ready_block(&fenced, &fp).replace(&row, &row.replace("v0.17.0", "v0.017.0"))),
        ),
        (
            "ready with a row state that is not the historical state",
            readme_with(&ready_block(&fenced, &fp).replace("historical key, retired for new releases", "current key")),
        ),
        (
            "ready without the pending machine sentence",
            readme_with(&ready_block(&fenced, &fp).replace(" The machine release key is pending:", " The machine key is pending:")),
        ),
        (
            "ready with a pending machine sentence beside a machine epoch",
            machine.replacen(
                "No other version has a release key.\n",
                &format!("No other version has a release key. The machine release key is pending: no version after {} has a release key yet.\n", tag_of(HISTORICAL_LAST)),
                1,
            ),
        ),
        (
            "ready with the allowed-signers line removed",
            readme_with(&ready_block(&fenced, &fp).replacen(
                &format!("\nThe allowed-signers line for v0.17.0 to {}:\n\n{fenced}\n", tag_of(HISTORICAL_LAST)),
                "",
                1,
            )),
        ),
        (
            "ready with a second line in the allowed-signers fence",
            readme_with(&ready_block(&fenced, &fp).replacen(
                &format!("{fenced}\n"),
                &format!("```text\n{signer}\n{}\n```\n", other_signer()),
                2,
            )),
        ),
        (
            "ready with another key's line in the allowed-signers block",
            machine.replacen(&signer_of(8), &signer_of(9), 1),
        ),
        (
            "ready with the historical line changed",
            readme_with(&ready_block(&fenced, &fp)).replacen(&signer, &other_signer(), 2),
        ),
        (
            "ready with text after the trust list",
            machine.replacen("### Install\n", "Extra words.\n\n### Install\n", 1),
        ),
        (
            "ready with a statement block whose JSON names an unlisted key",
            machine.replacen(&fp_of_signer(&signer_of(8)), "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", 1),
        ),
        (
            "ready with a statement signature that is not armored",
            machine.replacen("-----BEGIN SSH SIGNATURE-----", "BEGIN", 1),
        ),
        (
            "ready with a statement block missing",
            {
                let at = machine.find("\n#### revocation-v0.50.0.json").unwrap();
                let end = machine[at + 1..].find("\n#### transition-v0.60.0.json").unwrap() + at + 1;
                format!("{}{}", &machine[..at], &machine[end..])
            }
            .replace("#### transition-v0.60.0.json", "#### transition-v0.61.0.json"),
        ),
        (
            "ready with a statement JSON naming another public key",
            machine.replacen(
                &signer_of(8).rsplit(' ').next().unwrap().to_string(),
                signer_of(9).rsplit(' ').next().unwrap(),
                2,
            ),
        ),
        // the generated install blocks: every use is the selected line
        ("ready with one install block using a second key", {
            let at = combined
                .match_indices(signer.as_str())
                .nth(3)
                .expect("a use")
                .0;
            format!(
                "{}{}{}",
                &combined[..at],
                other_signer(),
                &combined[at + signer.len()..]
            )
        }),
        (
            "ready with install blocks and a second declaration",
            ready_with_install(&ready_block(&format!("{fenced}\n\n{fenced}"), &fp), &signer),
        ),
        (
            "ready with install blocks and a second declared key",
            ready_with_install(
                &ready_block(
                    &format!("{fenced}\n\n```text\n{}\n```", other_signer()),
                    &fp,
                ),
                &signer,
            ),
        ),
        (
            "ready with install blocks for a second key only",
            ready_with_install(&ready_block(&fenced, &fp), &other_signer()),
        ),
        (
            "ready with install blocks and a wrong fingerprint",
            ready_with_install(
                &ready_block(
                    &fenced,
                    "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                ),
                &signer,
            ),
        ),
        (
            "pending with install blocks",
            format!("{pending_readme}\n{}", install_section(&signer)),
        ),
        (
            "pending with the trust list",
            format!("{pending_readme}\n{trust}"),
        ),
    ];
    for (name, readme) in cases {
        assert!(
            trust_state_error(&readme).is_err(),
            "the pin accepted a bad README: {name}"
        );
    }

    // the bounds: the README, the key rows
    let huge = format!("{combined}\n{}", "x".repeat(README_MAX));
    assert!(
        trust_state_error(&huge)
            .unwrap_err()
            .contains("over the bound")
    );
    let rows = std::iter::repeat_n(format!("{row}\n"), ROWS_MAX + 1).collect::<String>();
    let many = readme_with(&ready_block(&fenced, &fp).replacen(&format!("{row}\n"), &rows, 1));
    assert!(
        trust_state_error(&many)
            .unwrap_err()
            .contains("over the bound")
    );
}

/// The fingerprint of a release signer line.
fn fp_of_signer(signer: &str) -> String {
    let b64 = signer.rsplit(' ').next().unwrap();
    ssh_key::PublicKey::from_openssh(&format!("ssh-ed25519 {b64}"))
        .unwrap()
        .fingerprint(ssh_key::HashAlg::Sha256)
        .to_string()
}

#[test]
fn pre2_preview_readme_preserves_the_dump_route() {
    let readme = read("README.md");
    // The note is the whole of the collapsed "About this preview" block at the bottom; a
    // pointer to it comes before the install steps.
    assert!(
        readme.contains(&format!(
            "<details>\n<summary><b>About this preview</b></summary>\n\n{PREVIEW_NOTE}\n</details>\n"
        )),
        "the preview note is not exact"
    );
    let at = |s: &str| readme.find(s).unwrap_or_else(|| panic!("missing {s}"));
    assert!(at("See \"About this preview\"") < at("## Install"));
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

#[test]
fn ain6_public_words_match_the_approved_description() {
    let (code, help, stderr) = run(&["--help"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        help.starts_with(&format!("{HEADLINE}\n\n{ABOUT}\n")),
        "{help}"
    );
    let readme = read("README.md");
    let top: Vec<_> = readme.lines().take(6).collect();
    assert!(top.contains(&HEADLINE) && top.contains(&ABOUT), "{top:?}");
    let agents = read("AGENTS.md");
    // the line number of the next AGENTS.md line is pinned by pre2_public_tree_names_no_private_project
    assert_eq!(
        agents.lines().nth(4),
        Some(format!("{HEADLINE} {ABOUT}").as_str())
    );
    for f in [&readme, &agents] {
        assert!(!f.contains("consistent while agents change them"));
    }
    assert!(readme.contains("rdy computes. Agents write. People decide."));
}

/// agi6 (D-GEKKY, D-NB9NE): the guide ships in the binary, so no step downloads it. The quick path
/// still comes first: install, a first project, and only then the optional verification.
#[test]
fn agi6_readme_uses_setup_and_no_guide_download() {
    let readme = read("README.md");
    let tried = readme
        .split_once("## Try it: your first project\n")
        .expect("the first-project section")
        .1
        .split_once("\n## ")
        .expect("the next section")
        .0;
    let steps: Vec<&str> = tried
        .lines()
        .filter(|l| {
            l.split_once(". **")
                .is_some_and(|(n, _)| n.parse::<u32>().is_ok())
        })
        .collect();
    assert_eq!(
        steps,
        [
            "1. **Make a project.**",
            "2. **Connect your agent.** For Claude Code:",
            "3. **Prepare your agent.**",
            "4. **Ask your agent for something real.** For example:",
            "5. **Review and approve it yourself.**",
            "6. **Read the result.**",
        ]
    );
    let step = tried
        .split_once("3. **Prepare your agent.**\n")
        .unwrap()
        .1
        .split_once("4. **Ask your agent")
        .unwrap()
        .0;
    assert!(step.contains("```sh\n   rdy setup\n   ```\n"), "{step}");
    for said in [
        "`rdy_instructions`",
        "The guide ships inside rdy, so you download nothing.",
        "[docs site](https://getrdy.dev)",
        "The `AGENTS.md` in this repository is a different file, for people who work on rdy itself.",
    ] {
        assert!(step.contains(said), "{said}: {step}");
    }
    // no line of the README downloads a guide
    for line in readme.lines() {
        for gone in [
            "Download `AGENTS.md`",
            "Download `CLAUDE.md`",
            "(or `CLAUDE.md`",
            "agent-guide.json",
        ] {
            assert!(!line.contains(gone), "{line}");
        }
    }
    assert!(!readme.contains("The guide in step 3"));
    assert!(!tried.contains("releases/tag"), "{tried}");
    // quick use first; verification stays optional and last
    let at = |what: &str| {
        readme
            .find(what)
            .unwrap_or_else(|| panic!("the README lacks {what}"))
    };
    let order = [
        at("## Install\n"),
        at("curl -fsSL https://getrdy.dev/install.sh | sh"),
        at("## Try it: your first project\n"),
        at("rdy setup\n"),
        at("## Quick start\n"),
        at("<summary><b>Verify the download (optional)</b></summary>"),
        at("You do not need this to use rdy."),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
}
