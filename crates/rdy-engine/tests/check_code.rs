//! ck4: the two code cases (D-JQD5X, D-54CKQ, D-RVM29). They read committed files of a linked
//! checkout through bounded git commands; nothing here runs what a decision or a file says.
use rdy_engine::check::{
    AgentCheck, BuiltCase, CaseInputs, build_case, parse_code_range, resolve_code_range,
};
use rdy_engine::{E_BAD_INPUT, E_IO, E_LIMIT, Materialized, Rdy, RdyError, ReadState};
use rdy_model::check_request::{CheckCode, CheckSubject};
use rdy_model::{Block, BlockId, FieldValue, Link, Op};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

const D: &str = "D-AAAA0";

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, path: &str, bytes: &[u8]) {
    let file = dir.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, bytes).unwrap();
}

/// Commits everything with `message` and returns the commit.
fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "commit",
            "-q",
            "--no-verify",
            "--allow-empty",
            "-m",
            message,
        ],
    );
    git(dir, &["rev-parse", "HEAD"])
}

fn cites(d: &str) -> String {
    format!("work\n\nImplements: {d}")
}

/// A project linked to one checkout, repo `app`, and an empty checkout at `repo`.
struct World {
    _project: tempfile::TempDir,
    _checkout: tempfile::TempDir,
    r: Rdy,
    repo: std::path::PathBuf,
}

fn world() -> World {
    let project = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let r = Rdy::init(project.path(), "rdy-standard").unwrap();
    git(checkout.path(), &["init", "-q", "--initial-branch=main"]);
    link_repos(project.path(), &[("app", checkout.path())]);
    let repo = std::fs::canonicalize(checkout.path()).unwrap();
    World {
        _project: project,
        _checkout: checkout,
        r,
        repo,
    }
}

fn link_repos(project: &Path, repos: &[(&str, &Path)]) {
    let names: Vec<&str> = repos.iter().map(|r| r.0).collect();
    let mut yaml = std::fs::read_to_string(project.join(".rdy.yaml")).unwrap();
    yaml.push_str(&format!("code_repos: [{}]\n", names.join(", ")));
    std::fs::write(project.join(".rdy.yaml"), yaml).unwrap();
    std::fs::create_dir_all(project.join(".rdy")).unwrap();
    let mut local = String::from("code_paths:\n");
    for (n, p) in repos {
        local.push_str(&format!("  {n}: {}\n", p.display()));
    }
    std::fs::write(project.join(".rdy/local.yaml"), local).unwrap();
}

fn block(i: &str, ty: &str, body: &str) -> Block {
    Block {
        id: id(i),
        block_type: ty.into(),
        parent: None,
        order: "a0".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn decision(i: &str, confirmation: Option<&str>) -> Block {
    let mut d = block(i, "decision", "Why it holds.");
    let mut put = |k: &str, v: &str| {
        d.fields.insert(k.into(), FieldValue::Str(v.into()));
    };
    put("status", "accepted");
    put("statement", "We keep the rule.");
    if let Some(c) = confirmation {
        put("confirmation", c);
    }
    d
}

fn implementation(i: &str, of: &str) -> Block {
    let mut b = block(i, "claim", "It follows.");
    b.links.entry("implements".into()).or_default().push(Link {
        target: id(of),
        part: None,
    });
    b
}

fn state(blocks: Vec<Block>) -> Materialized {
    let ops: Vec<Op> = blocks
        .into_iter()
        .map(|block| Op::Create { block })
        .collect();
    Materialized::default().with(&ops)
}

fn diff_subject(from: &str, to: &str) -> CheckSubject {
    CheckSubject {
        blocks: vec![id(D)],
        change: None,
        version: None,
        code: Some(CheckCode {
            repo: "app".into(),
            from: from.into(),
            to: to.into(),
        }),
    }
}

fn confirm_subject() -> CheckSubject {
    CheckSubject {
        blocks: vec![id(D)],
        change: None,
        version: None,
        code: None,
    }
}

fn build(
    st: &ReadState,
    check: AgentCheck,
    subject: &CheckSubject,
    m: &Materialized,
) -> Result<BuiltCase, RdyError> {
    build_case(
        check,
        subject,
        CaseInputs {
            st,
            before: m,
            after: m,
            changes: &[],
        },
    )
}

fn code_row<'a>(case: &'a Value, path: &str) -> &'a Value {
    case["code"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == path)
        .unwrap_or_else(|| panic!("no code row {path} in {:#}", case["code"]))
}

fn paths(case: &Value) -> Vec<String> {
    case["code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn ck4_diff_reads_only_anchored_committed_files() {
    let w = world();
    let dir = &w.repo;
    write(dir, "src/kept.rs", b"one\ntwo\n");
    write(dir, "src/by_impl.rs", b"impl one\n");
    write(dir, "src/gone.rs", b"gone\n");
    write(dir, "other.rs", b"other\n");
    git(dir, &["add", "src/kept.rs", "src/gone.rs"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", &cites(D)]);
    git(dir, &["add", "src/by_impl.rs"]);
    git(
        dir,
        &["commit", "-q", "--no-verify", "-m", &cites("c-1MP00")],
    );
    let from = commit(dir, "unrelated");

    // a hostile diff driver, a textconv filter and an external diff: none may run
    let marker = dir.join("PWNED");
    let script = dir.join("evil.sh");
    write(
        dir,
        "evil.sh",
        format!("#!/bin/sh\necho ran > {}\n", marker.display()).as_bytes(),
    );
    git(dir, &["update-index", "--add", "--chmod=+x", "evil.sh"]);
    let evil = script.display().to_string();
    git(dir, &["config", "diff.evil.command", &evil]);
    git(dir, &["config", "diff.evil.textconv", &evil]);
    git(dir, &["config", "diff.external", &evil]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    write(dir, ".gitattributes", b"*.rs diff=evil\n");
    write(dir, "src/kept.rs", b"one\nTWO\n");
    write(dir, "other.rs", b"other changed\n");
    git(dir, &["add", "-A"]);
    git(dir, &["rm", "-q", "--cached", "src/gone.rs"]);
    std::fs::remove_file(dir.join("src/gone.rs")).unwrap();
    git(dir, &["add", "-A"]);
    let to = commit(dir, "unrelated second");

    // later working-tree dirt, staged and untracked files
    write(dir, "src/kept.rs", b"DIRTY\n");
    write(dir, "src/untracked.rs", b"untracked\n");

    let st = w.r.read();
    let m = state(vec![decision(D, None), implementation("c-1MP00", D)]);
    let got = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject(&from, &to),
        &m,
    )
    .unwrap();
    assert!(!marker.exists(), "no git attribute or driver ran");
    assert_eq!(
        paths(&got.case),
        ["src/by_impl.rs", "src/gone.rs", "src/kept.rs"],
        "the decision's and its implementation's anchors at both endpoints, sorted"
    );
    let kept = code_row(&got.case, "src/kept.rs");
    assert_eq!(kept["repo"], "app");
    assert_eq!(kept["from"], from);
    assert_eq!(kept["to"], to);
    let text = kept["text"].as_str().unwrap();
    assert!(text.contains("-two\n") && text.contains("+TWO\n"), "{text}");
    assert!(!text.contains("DIRTY"), "{text}");
    assert_eq!(kept["code_cut"], false);
    assert_eq!(
        kept["before_blob"],
        git(dir, &["rev-parse", &format!("{from}:src/kept.rs")])
    );
    assert_eq!(
        kept["after_blob"],
        git(dir, &["rev-parse", &format!("{to}:src/kept.rs")])
    );
    let gone = code_row(&got.case, "src/gone.rs");
    assert!(
        gone["after_blob"].is_null(),
        "a deleted file has no after blob"
    );
    assert!(gone["text"].as_str().unwrap().contains("-gone\n"));
    let same = code_row(&got.case, "src/by_impl.rs");
    assert_eq!(same["before_blob"], same["after_blob"]);
    assert_eq!(same["text"], "");
    assert!(!got.case.to_string().contains("other changed"));
    assert_eq!(got.case["kind"], "diff_contradicts");
    assert_eq!(got.subject_blocks, [id(D)]);
    assert_eq!(
        got.case["blocks"][0]["role"], "subject",
        "the decision is the case's one block row"
    );

    // a binary file's diff shows no text; its two blobs still bind the case
    write(dir, "src/bin.rs", b"\0one");
    git(dir, &["add", "-A", "src/bin.rs"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", &cites(D)]);
    let was = git(dir, &["rev-parse", "HEAD"]);
    write(dir, "src/bin.rs", b"\0two");
    git(dir, &["add", "-A", "src/bin.rs"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", &cites(D)]);
    let got = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject(&was, "HEAD"),
        &m,
    )
    .unwrap();
    let bin = code_row(&got.case, "src/bin.rs");
    assert_eq!(bin["text"], "");
    assert_eq!(bin["code_cut"], true);
    assert_ne!(bin["before_blob"], bin["after_blob"]);
    assert!(got.case["omitted"]["code_bytes"].as_u64().unwrap() > 0);

    // an added file has no before blob; a symbolic name resolves to its full commit
    write(dir, "src/new.rs", b"new\n");
    git(dir, &["add", "-A", "src/new.rs"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", &cites(D)]);
    let got = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject(&from, "main"),
        &m,
    )
    .unwrap();
    let added = code_row(&got.case, "src/new.rs");
    assert!(added["before_blob"].is_null());
    assert_eq!(added["to"], git(dir, &["rev-parse", "main"]));
}

#[test]
fn ck4_confirmation_is_evidence_not_a_command() {
    let w = world();
    let dir = &w.repo;
    write(dir, "src/a.rs", b"fn a() {}\n");
    commit(dir, &cites(D));
    write(dir, "src/b.rs", b"fn b() {}\n");
    commit(dir, &cites("c-1MP00"));
    let marker = dir.join("PWNED");
    let command = format!("$(touch {m}); touch {m} && echo done", m = marker.display());
    let st = w.r.read();
    let m = state(vec![
        decision(D, Some(&command)),
        implementation("c-1MP00", D),
    ]);
    let got = build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap();
    assert!(!marker.exists(), "the confirmation text is never run");
    let c = &got.case;
    assert_eq!(c["kind"], "confirmation");
    let rows = c["blocks"].as_array().unwrap();
    assert_eq!(rows[0]["id"], D);
    assert_eq!(rows[0]["role"], "subject");
    assert_eq!(rows[1]["id"], "c-1MP00");
    assert_eq!(rows[1]["role"], "implementation");
    let text = &rows[0]["after"]["text"];
    assert_eq!(
        text,
        &json!([
            {"field": "statement", "text": "We keep the rule."},
            {"field": "body", "text": "Why it holds."},
            {"field": "confirmation", "text": command},
        ])
    );
    let head = git(dir, &["rev-parse", "HEAD"]);
    let a = code_row(c, "src/a.rs");
    assert_eq!(a["text"], "fn a() {}\n");
    assert_eq!(a["repo"], "app");
    assert!(a["from"].is_null() && a["before_blob"].is_null());
    assert_eq!(a["to"], head);
    assert_eq!(a["after_blob"], git(dir, &["rev-parse", "HEAD:src/a.rs"]));
    assert_eq!(code_row(c, "src/b.rs")["text"], "fn b() {}\n");

    // a binary file binds by its blob and shows no text
    write(dir, "src/bin.dat", b"\0\x01\x02binary");
    commit(dir, &cites(D));
    let got = build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap();
    let bin = code_row(&got.case, "src/bin.dat");
    assert_eq!(bin["text"], "");
    assert_eq!(bin["code_cut"], true);
    assert_eq!(got.case["omitted"]["code_bytes"], 9);

    // a decision with no text, or blank text, is a wrong subject
    for blank in [None, Some("  \n")] {
        let m = state(vec![decision(D, blank)]);
        let e = build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT);
        assert_eq!(
            e.message,
            format!(
                "confirmation takes a decision with confirmation text; {D} is a decision without confirmation text"
            )
        );
    }
}

#[test]
fn ck4_code_case_hash_binds_heads_blobs_and_omissions() {
    let w = world();
    let dir = &w.repo;
    for n in 0..65 {
        write(dir, &format!("f{n:02}.rs"), b"start\n");
    }
    let from = commit(dir, &cites(D));
    for n in 0..65 {
        write(dir, &format!("f{n:02}.rs"), b"changed\n");
    }
    let to = commit(dir, &cites(D));
    let st = w.r.read();
    let m = state(vec![decision(D, Some("A test passes."))]);
    let diff = |to: &str| {
        build(
            &st,
            AgentCheck::DiffContradicts,
            &diff_subject(&from, to),
            &m,
        )
        .unwrap()
    };
    let confirm = || build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap();
    let (d0, c0) = (diff(&to), confirm());
    assert_eq!(d0.hash, diff(&to).hash, "a case is deterministic");

    // working-tree dirt, staged edits and untracked files leave both cases
    write(dir, "f00.rs", b"dirty\n");
    write(dir, "untracked.rs", b"untracked\n");
    git(dir, &["add", "f01.rs"]);
    assert_eq!(d0.hash, diff(&to).hash);
    assert_eq!(c0.hash, confirm().hash);
    git(dir, &["checkout", "-q", "--", "f00.rs"]);
    std::fs::remove_file(dir.join("untracked.rs")).unwrap();

    // a commit that changes only an omitted file moves the hash; the rows shown stay
    write(dir, "f64.rs", b"changed twice\n");
    let to2 = commit(dir, &cites(D));
    let d1 = diff(&to2);
    assert_eq!(d0.case["omitted"], d1.case["omitted"]);
    assert_eq!(
        d0.case["code"][0]["text"], d1.case["code"][0]["text"],
        "the shown file is the same"
    );
    assert_ne!(d0.case["inputs_hash"], d1.case["inputs_hash"]);
    assert_ne!(d0.hash, d1.hash);
    let c1 = confirm();
    assert_eq!(
        c0.case["omitted"]["code_files"],
        c1.case["omitted"]["code_files"]
    );
    assert_ne!(c0.hash, c1.hash);

    // an empty commit moves a head and so the hash, though every blob is the same
    commit(dir, "empty");
    assert_ne!(c1.hash, confirm().hash);
}

/// The exact stdout of the diff a case reads for `path`.
fn diff_len(dir: &Path, from: &str, to: &str, path: &str) -> usize {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "--literal-pathspecs",
            "diff-tree",
            "-r",
            "-p",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "--unified=3",
            from,
            to,
            "--",
            path,
        ])
        .output()
        .unwrap();
    out.stdout.len()
}

/// Two commits of `big.txt`, whose diff prints exactly `want` bytes.
fn diff_of_size(dir: &Path, want: usize) -> (String, String) {
    write(dir, "big.txt", b"a\n");
    let from = commit(dir, &cites(D));
    write(dir, "big.txt", b"b\n");
    let probe = commit(dir, &cites(D));
    let small = diff_len(dir, &from, &probe, "big.txt");
    assert!(want > small);
    // the one-line file grows by the bytes the diff still lacks; the blob id keeps its length
    let body = format!("{}\n", "b".repeat(want - small + 1));
    write(dir, "big.txt", body.as_bytes());
    let to = commit(dir, &cites(D));
    assert_eq!(diff_len(dir, &from, &to, "big.txt"), want);
    (from, to)
}

#[test]
fn ck4_code_caps_are_fixed_and_children_are_reaped() {
    // 64 files and one past, in both code recipes
    let w = world();
    let dir = &w.repo;
    for n in 0..64 {
        write(dir, &format!("f{n:02}.rs"), b"x\n");
    }
    let from = commit(dir, &cites(D));
    let st = w.r.read();
    let m = state(vec![decision(D, Some("A test passes."))]);
    let confirm = || build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap();
    let got = confirm();
    assert_eq!(got.case["code"].as_array().unwrap().len(), 64);
    assert_eq!(got.case["omitted"]["code_files"], 0);
    write(dir, "f64.rs", b"x\n");
    let to = commit(dir, &cites(D));
    let got = confirm();
    assert_eq!(got.case["code"].as_array().unwrap().len(), 64);
    assert_eq!(got.case["omitted"]["code_files"], 1);
    let got = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject(&from, &to),
        &m,
    )
    .unwrap();
    assert_eq!(got.case["code"].as_array().unwrap().len(), 64);
    assert_eq!(got.case["omitted"]["code_files"], 1);
    drop(st);

    // 16,384 excerpt bytes and one past; the file total of 65,536 after four whole files
    let w = world();
    let dir = &w.repo;
    write(dir, "a.rs", "x".repeat(16_384).as_bytes());
    write(dir, "b.rs", "x".repeat(16_385).as_bytes());
    write(dir, "c.rs", "x".repeat(16_384).as_bytes());
    write(dir, "d.rs", "x".repeat(16_384).as_bytes());
    write(dir, "e.rs", b"late");
    // a file past the read bound keeps its blob identity and refuses nothing
    write(dir, "z.rs", "x".repeat(1_048_577).as_bytes());
    commit(dir, &cites(D));
    let st = w.r.read();
    let got = build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap();
    let c = &got.case;
    assert_eq!(code_row(c, "a.rs")["text"].as_str().unwrap().len(), 16_384);
    assert_eq!(code_row(c, "a.rs")["code_cut"], false);
    assert_eq!(code_row(c, "b.rs")["text"].as_str().unwrap().len(), 16_384);
    assert_eq!(code_row(c, "b.rs")["code_cut"], true);
    assert_eq!(
        code_row(c, "e.rs")["text"],
        "",
        "65,536 bytes are already shown"
    );
    assert_eq!(code_row(c, "e.rs")["code_cut"], true);
    assert_eq!(code_row(c, "z.rs")["text"], "");
    assert_eq!(code_row(c, "z.rs")["code_cut"], true);
    assert_eq!(c["omitted"]["code_bytes"], 1 + 4 + 1_048_577);
    drop(st);

    // code text shares the case's cap, and block text comes first
    let w = world();
    let dir = &w.repo;
    write(dir, "a.rs", b"code");
    commit(dir, &cites(D));
    let st = w.r.read();
    let mut blocks = vec![decision(D, Some("A test passes."))];
    for n in 0..16 {
        let mut b = implementation(&format!("c-1MP{n:02}"), D);
        b.body = "x".repeat(16_384);
        blocks.push(b);
    }
    let full = state(blocks);
    let got = build(&st, AgentCheck::Confirmation, &confirm_subject(), &full).unwrap();
    assert_eq!(code_row(&got.case, "a.rs")["text"], "");
    assert_eq!(code_row(&got.case, "a.rs")["code_cut"], true);
    assert_eq!(got.case["omitted"]["code_bytes"], 4);
    drop(st);

    // 65,536 retained diff bytes and one past
    let w = world();
    let dir = &w.repo;
    let (from, to) = diff_of_size(dir, 65_536);
    let st = w.r.read();
    let m = state(vec![decision(D, None)]);
    let got = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject(&from, &to),
        &m,
    )
    .unwrap();
    assert_eq!(
        code_row(&got.case, "big.txt")["text"]
            .as_str()
            .unwrap()
            .len(),
        65_536
    );
    assert_eq!(code_row(&got.case, "big.txt")["code_cut"], false);
    assert_eq!(got.case["omitted"]["code_bytes"], 0);
    let (from, to) = diff_of_size(dir, 65_537);
    let got = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject(&from, &to),
        &m,
    )
    .unwrap();
    assert_eq!(
        code_row(&got.case, "big.txt")["text"]
            .as_str()
            .unwrap()
            .len(),
        65_536
    );
    assert_eq!(code_row(&got.case, "big.txt")["code_cut"], true);
    assert_eq!(got.case["omitted"]["code_bytes"], 1);

    // a reached drain cap is the existing refusal and no case; exactly the cap is a case
    let (from, to) = diff_of_size(dir, 1_048_576);
    let got = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject(&from, &to),
        &m,
    )
    .unwrap();
    assert_eq!(
        got.case["omitted"]["code_bytes"],
        1_048_576 - 65_536,
        "the bytes past the retained 65,536"
    );
    let (from, to) = diff_of_size(dir, 1_048_577);
    let e = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject(&from, &to),
        &m,
    )
    .unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "code evidence exceeds 1048576 output bytes");
    assert_eq!(e.fix, "use a smaller linked checkout");
}

fn assert_refused(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(e.code, code, "{e:?}");
    assert_eq!(e.message, message);
    assert_eq!(e.fix, fix);
}

#[test]
fn ck4_code_refusals_are_exact() {
    let w = world();
    let dir = &w.repo;
    write(dir, "src/a.rs", b"a\n");
    let c1 = commit(dir, &cites(D));
    let root = w.r.root().to_path_buf();
    let st = w.r.read();
    let m = state(vec![decision(D, Some("A test."))]);

    // a bad range: every separator shape, and each bound
    let bad = "code range must be <repo>@<from>..<to>";
    let fix = "name a linked repo and two commits";
    let long_repo = format!("{}@a..b", "r".repeat(257));
    let long_rev = format!("app@{}..b", "a".repeat(65));
    for text in [
        "",
        "app",
        "app@main",
        "app..main",
        "@a..b",
        "app@..b",
        "app@a..",
        "app@a..b..c",
        "app@a...b",
        "a@b@c..d",
        long_repo.as_str(),
        long_rev.as_str(),
    ] {
        assert_refused(&parse_code_range(text).unwrap_err(), E_BAD_INPUT, bad, fix);
    }
    let ok = parse_code_range(&format!(
        "{}@{}..{}",
        "r".repeat(256),
        "a".repeat(64),
        "b".repeat(64)
    ));
    assert!(ok.is_ok(), "the bounds themselves are allowed");
    assert_eq!(
        parse_code_range("app@a..b").unwrap(),
        CheckCode {
            repo: "app".into(),
            from: "a".into(),
            to: "b".into()
        }
    );
    let resolved = resolve_code_range(
        AgentCheck::DiffContradicts,
        &root,
        &format!("app@{c1}..main"),
    )
    .unwrap();
    assert_eq!(
        (resolved.from.as_str(), resolved.to.as_str()),
        (c1.as_str(), c1.as_str())
    );

    // a repo with no readable linked checkout
    let no_checkout = |repo: &str| {
        (
            format!("check diff_contradicts needs a readable checkout for {repo}"),
            format!(
                "point code_paths at a checkout of {repo}, then run or request the check again"
            ),
        )
    };
    let (msg, fix) = no_checkout("elsewhere");
    let mut s = diff_subject(&c1, &c1);
    s.code.as_mut().unwrap().repo = "elsewhere".into();
    assert_refused(
        &build(&st, AgentCheck::DiffContradicts, &s, &m).unwrap_err(),
        E_BAD_INPUT,
        &msg,
        &fix,
    );

    // a revision that resolves to no commit, option-shaped ones included
    let blob = git(dir, &["rev-parse", "HEAD:src/a.rs"]);
    for rev in [
        "no-such-rev",
        "--all",
        "--output=/tmp/ck4-should-not-exist",
        "-n1",
        blob.as_str(),
    ] {
        let e = build(
            &st,
            AgentCheck::DiffContradicts,
            &diff_subject(&c1, rev),
            &m,
        )
        .unwrap_err();
        assert_refused(
            &e,
            E_BAD_INPUT,
            &format!("code revision {rev} is not a commit in app"),
            "name a commit that exists in the linked checkout",
        );
    }
    assert!(!Path::new("/tmp/ck4-should-not-exist").exists());
    let e = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject("--all", &c1),
        &m,
    )
    .unwrap_err();
    assert_eq!(e.message, "code revision --all is not a commit in app");
    let mut s = diff_subject(&c1, &c1);
    s.code.as_mut().unwrap().to = "x".repeat(65);
    assert_refused(
        &build(&st, AgentCheck::DiffContradicts, &s, &m).unwrap_err(),
        E_BAD_INPUT,
        bad,
        "name a linked repo and two commits",
    );

    // no anchored file: both recipes
    let plain = state(vec![decision("D-BBBB0", Some("A test."))]);
    let mut s = diff_subject(&c1, &c1);
    s.blocks = vec![id("D-BBBB0")];
    let fix = "add code anchors to the decision or its implementation blocks";
    assert_refused(
        &build(&st, AgentCheck::DiffContradicts, &s, &plain).unwrap_err(),
        E_BAD_INPUT,
        "check diff_contradicts has no anchored code for D-BBBB0",
        fix,
    );
    let mut s = confirm_subject();
    s.blocks = vec![id("D-BBBB0")];
    assert_refused(
        &build(&st, AgentCheck::Confirmation, &s, &plain).unwrap_err(),
        E_BAD_INPUT,
        "check confirmation has no anchored code for D-BBBB0",
        fix,
    );
    drop(st);

    // an unreadable checkout: a folder that is no repository, then one that has gone. Every
    // listed repo counts, so a case never binds to the checkouts one machine happens to hold.
    let project = tempfile::tempdir().unwrap();
    let r = Rdy::init(project.path(), "rdy-standard").unwrap();
    let plain_dir = tempfile::tempdir().unwrap();
    link_repos(project.path(), &[("app", plain_dir.path())]);
    let st = r.read();
    let host_path = plain_dir.path().display().to_string();
    let e = build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap_err();
    assert_refused(
        &e,
        E_BAD_INPUT,
        "check confirmation needs a readable checkout for app",
        "point code_paths at a checkout of app, then run or request the check again",
    );
    assert!(!e.message.contains(&host_path) && !e.fix.contains(&host_path));
    let e = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject("a", "b"),
        &m,
    )
    .unwrap_err();
    assert_eq!(
        e.message,
        "check diff_contradicts needs a readable checkout for app"
    );
    drop(plain_dir);
    let e = build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap_err();
    assert_eq!(
        e.message,
        "check confirmation needs a readable checkout for app"
    );
    drop(st);

    // a repo listed with no path on this machine
    let project = tempfile::tempdir().unwrap();
    let r = Rdy::init(project.path(), "rdy-standard").unwrap();
    let mut yaml = std::fs::read_to_string(project.path().join(".rdy.yaml")).unwrap();
    yaml.push_str("code_repos: [app]\n");
    std::fs::write(project.path().join(".rdy.yaml"), yaml).unwrap();
    let st = r.read();
    let e = build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap_err();
    assert_eq!(
        e.message,
        "check confirmation needs a readable checkout for app"
    );
    drop(st);

    // no linked repo at all has no anchored code
    let project = tempfile::tempdir().unwrap();
    let r = Rdy::init(project.path(), "rdy-standard").unwrap();
    let st = r.read();
    let e = build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap_err();
    assert_eq!(
        e.message,
        format!("check confirmation has no anchored code for {D}")
    );
    drop(st);

    // a decision alone is no diff subject; a block that is no decision neither
    let st = w.r.read();
    let mut s = diff_subject(&c1, &c1);
    s.code = None;
    let e = build(&st, AgentCheck::DiffContradicts, &s, &m).unwrap_err();
    assert_eq!(
        e.message,
        format!(
            "diff_contradicts takes a decision and a code range; {D} is a decision without a code range"
        )
    );
    let claim = state(vec![block(D, "claim", "x")]);
    let e = build(
        &st,
        AgentCheck::DiffContradicts,
        &diff_subject(&c1, &c1),
        &claim,
    )
    .unwrap_err();
    assert_eq!(
        e.message,
        format!("diff_contradicts takes a decision and a code range; {D} is a claim")
    );

    // git itself is absent: a child of this test binary, with an empty PATH, opens the project
    // this test has let go of
    drop(st);
    drop(w.r);
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ck4_absent_git_child",
            "--nocapture",
            "--ignored",
        ])
        .env("PATH", "")
        .env("CK4_PROJECT", &root)
        .output()
        .unwrap();
    assert!(
        child.status.success() && String::from_utf8_lossy(&child.stdout).contains("1 passed"),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}

/// Run by [`ck4_code_refusals_are_exact`] alone, in a process that cannot find git.
#[test]
#[ignore = "a child process of ck4_code_refusals_are_exact"]
fn ck4_absent_git_child() {
    let root = std::env::var_os("CK4_PROJECT").expect("run by ck4_code_refusals_are_exact");
    let r = Rdy::open(Path::new(&root)).unwrap();
    let st = r.read();
    let m = state(vec![decision(D, Some("A test."))]);
    let e = build(&st, AgentCheck::Confirmation, &confirm_subject(), &m).unwrap_err();
    assert_refused(
        &e,
        E_IO,
        "git is not installed or not on PATH",
        "install git: rdy reads linked code repos with git log",
    );
}
