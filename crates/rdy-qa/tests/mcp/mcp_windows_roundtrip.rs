//! wx26a: what the chat path writes, the CLI reads back as the same bytes, on every platform. Text
//! with a backslash path, a non-ASCII letter and an astral character crosses the stdio server, the
//! daemon and the CLI unchanged; the chat-first path (write, propose, approve, read, export)
//! gives the CLI's replies and the CLI's files; and the bytes a signature and an export hold are
//! the same on Windows and Unix. The expected values are the files under
//! `fixtures/windows/`; `RDY_QA_BLESS=1` rewrites them, and a Windows run never does.
use rdy_engine::signature_check::SignatureSubject;
use rdy_model::signature::StatementSource;
use rdy_model::{Provenance, RepoId, Role};
use rdy_qa::daemon::Served;
use rdy_qa::golden;
use rdy_qa::mcp::McpClient;
use rdy_qa::signing::{TestKey, sign_record, vector};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/windows")
        .join(name)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A writer session on the served project's daemon, in the view `run`.
fn agent(s: &Served, run: &str) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": run, "role": "writer"}),
    );
    assert!(r.is_ok(), "{r:?}");
    c
}

/// Every file under `dir` as `(slash path, SHA-256 hex)`, sorted by path. A path is relative and
/// slash-separated on every platform.
fn tree_digests(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("list a directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(dir)
                .expect("under the tree")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let bytes = std::fs::read(&path).expect("read a file");
            out.push((rel, hex(&Sha256::digest(&bytes))));
        }
    }
    out.sort();
    out
}

fn digest_text(rows: &[(String, String)]) -> String {
    rows.iter().map(|(p, h)| format!("{h}  {p}\n")).collect()
}

/// Compares `got` with the fixture file `name`, or rewrites the file under `RDY_QA_BLESS=1` on a
/// platform that is not Windows.
fn pinned(name: &str, got: &str) {
    let path = fixture(name);
    if std::env::var_os("RDY_QA_BLESS").is_some() && !cfg!(windows) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, got).unwrap();
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; bless with RDY_QA_BLESS=1", path.display()));
    // a checkout may have turned LF into CRLF; the pinned bytes are the LF text
    assert_eq!(got, want.replace("\r\n", "\n"), "{name}");
}

/// Text that crosses the boundaries a Windows run can change: a drive path, a UNC path, a
/// verbatim path, a non-ASCII letter, a letter outside the BMP and trailing white space.
const TEXTS: [&str; 5] = [
    "Caf\u{e9} at C:\\Users\\qa\\notes.txt",
    "share \\\\server\\share\\dir",
    "verbatim \\\\?\\C:\\very\\long\\path",
    "clef \u{1d11e} and a tab\tand two spaces  ",
    "forward C:/Users/qa/notes.txt",
];

#[test]
fn wx26a_chat_write_and_cli_read_agree_on_windows() {
    let s = Served::new();
    let mut c = agent(&s, "w1");
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "native", "title": "Native", "doc_type": "page"}}),
    ];
    for (n, text) in TEXTS.iter().enumerate() {
        ops.push(json!({"op": "create", "type": "prose", "id": format!("$p{n}"), "parent": "$d", "body": text}));
    }
    let w = c.call("rdy_write", json!({"ops": ops}));
    assert!(w.is_ok(), "{w:?}");
    let created = w.ok()["created"].clone();
    for (n, text) in TEXTS.iter().enumerate() {
        let id = created[format!("$p{n}")]
            .as_str()
            .expect("an id")
            .to_string();
        let chat = c.call("rdy_context", json!({"id": id})).ok();
        let (ok, cli) = s.json(&["--json", "show", &id, "--scope", "w1"]);
        assert!(ok, "{cli}");
        assert_eq!(chat["body"], *text, "chat read {n}");
        assert_eq!(cli["body"], *text, "CLI read {n}");
        for key in ["block_type", "parent", "version", "id"] {
            assert_eq!(chat[key], cli[key], "{key} of block {n}");
        }
    }
    // the CLI's tree of the same view lists the doc the chat made
    let (ok, ls) = s.json(&["--json", "ls", "--scope", "w1"]);
    assert!(ok, "{ls}");
    assert!(ls.to_string().contains("native"), "{ls}");
}

#[test]
fn wx26a_chat_first_tutorial_matches_cli_parity() {
    let s = Served::new();
    let mut c = agent(&s, "t1");
    // 1. write a page, 2. propose it, 3. the person approves it, 4. read it, 5. export it
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "welcome", "title": "Welcome", "doc_type": "page"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "First steps"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Open C:\\work\\docs and read the page."}
            ]}),
        )
        .ok();
    let page = w["created"]["$p"].as_str().unwrap().to_string();
    let p = c
        .call(
            "rdy_propose",
            json!({"title": "Welcome page", "summary": "Adds the welcome page and its first steps."}),
        )
        .ok();
    let r = p["ref"]
        .as_str()
        .or_else(|| p["proposal"]["ref"].as_str())
        .unwrap_or_else(|| panic!("a ref in {p}"))
        .to_string();
    let out = s.rdy(&["approve", &r]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // the same block, in canon, from the chat and from the CLI
    let chat = c.call("rdy_context", json!({"id": page})).ok();
    let (ok, cli) = s.json(&["--json", "show", &page]);
    assert!(ok, "{cli}");
    for key in ["body", "block_type", "parent", "version", "id"] {
        assert_eq!(chat[key], cli[key], "{key}");
    }
    assert_eq!(cli["body"], "Open C:\\work\\docs and read the page.");

    // the instructions: one reply from the tool and from the command
    let chat = c.call("rdy_instructions", json!({})).ok();
    let (ok, cli) = s.json(&["--json", "instructions"]);
    assert!(ok, "{cli}");
    assert_eq!(chat, cli);

    // the exported Markdown tree: the same files, byte for byte, from the tool and from the command
    let by_tool = s.dir.path().join("by-tool");
    let by_cli = s.dir.path().join("by-cli");
    let v = c
        .call("rdy_export", json!({"out": "by-tool", "format": "md"}))
        .ok();
    assert_eq!(v["format"], "md", "{v}");
    let out = s.rdy(&[
        "export",
        "--format",
        "md",
        "--out",
        by_cli.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (tool, cli) = (tree_digests(&by_tool), tree_digests(&by_cli));
    assert!(!tool.is_empty());
    assert_eq!(tool, cli);
    assert!(
        tool.iter().all(|(p, _)| !p.contains('\\')),
        "slash paths: {tool:?}"
    );
}

/// The seed of the exported project's ids.
const GOLDEN_SEED: u64 = 0x26a0_26a0;

/// The golden corpus exported as Markdown, as a digest of every file.
fn golden_export_digests() -> Vec<(String, String)> {
    // seeded ids and a pinned clock (the golden project's own): the tree is the same each run
    rdy_model::seed_ids_for_tests(GOLDEN_SEED);
    let (dir, r, _ids) = golden::project();
    drop(r);
    rdy_model::unseed_ids_for_tests();
    let out = tempfile::tempdir().expect("tempdir");
    let target = out.path().join("md");
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(["--no-daemon", "--project"])
        .arg(dir.path())
        .args(["export", "--format", "md", "--out"])
        .arg(&target);
    let done = rdy_qa::fixtures::output_within(&mut cmd, std::time::Duration::from_secs(120))
        .expect("rdy export ran");
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    tree_digests(&target)
}

#[test]
fn wx26a_signed_and_exported_bytes_are_platform_independent() {
    // an SSHSIG over a fixed statement: Ed25519 is deterministic, so the bytes are the same on
    // every platform
    let v = vector(false);
    let SignatureSubject::Landing { hash, record } = &v.context.subject else {
        panic!("the vector signs a landing");
    };
    let signed_by = Provenance {
        actor: "human:qa".into(),
        run: None,
        role: Role::Approver,
        at_ms: 1,
    };
    let sig = sign_record(
        *hash,
        StatementSource::Approval(record),
        RepoId([0x14; 16]),
        signed_by,
        TestKey::Person,
    );
    let signed = json!({
        "namespace": sig.namespace,
        "statement": String::from_utf8_lossy(&sig.statement),
        "sig_hex": hex(&sig.sig),
    });
    pinned(
        "signed-record.json",
        &format!("{}\n", serde_json::to_string_pretty(&signed).unwrap()),
    );

    // the exported tree: the same paths and bytes on every platform
    let rows = golden_export_digests();
    assert!(!rows.is_empty());
    pinned("export-md.sha256", &digest_text(&rows));
}
