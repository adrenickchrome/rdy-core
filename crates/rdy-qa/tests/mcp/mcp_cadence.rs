//! di11 (FW-PC1, D-0GGW9; D-JV6Z8): `propose.cadence` and `propose.changes` in the levels,
//! `propose_due` on write replies, what `rdy_session` returns about the cadence and the queue, and
//! the approver refusal that points to `rdy_answer`.
use rdy_qa::fixtures::*;
use rdy_qa::intake::batch_project;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::path::Path;

/// A project with a landed doc; `config` is appended to `.rdy.yaml` and `local` is written to
/// `.rdy/local.yaml` (when given). Returns the section's id and the directory.
fn configured(config: &str, local: Option<&str>) -> (tempfile::TempDir, String) {
    let (dir, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    drop(r);
    append(dir.path(), ".rdy.yaml", config);
    if let Some(l) = local {
        append(dir.path(), ".rdy/local.yaml", l);
    }
    (dir, section)
}

fn append(dir: &Path, file: &str, text: &str) {
    let path = dir.join(file);
    let mut old = std::fs::read_to_string(&path).unwrap_or_default();
    if !old.is_empty() && !old.ends_with('\n') {
        old.push('\n');
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("{old}{text}")).unwrap();
}

fn session(c: &mut McpClient, role: &str) -> Value {
    c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "w", "role": role}),
    )
    .ok()
}

fn write_claim(c: &mut McpClient, section: &str, body: &str) -> Value {
    c.call(
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "claim", "parent": section, "body": body}]}),
    )
    .ok()
}

fn warning(s: &Value, message: &str) -> Value {
    s["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("no warnings in {s}"))
        .iter()
        .find(|w| w["message"] == message)
        .unwrap_or_else(|| panic!("no warning {message:?} in {s}"))
        .clone()
}

#[test]
fn di11_rdy_session_returns_the_cadence() {
    let (dir, _s) = configured("", None);
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "writer");
    assert_eq!(s["propose"], json!({"cadence": "break"}), "{s}");
    assert!(s.get("warnings").is_none(), "{s}");
    drop(c);
    for word in ["task", "asked", "break"] {
        let (dir, _s) = configured(&format!("propose:\n  cadence: {word}\n"), None);
        let mut c = McpClient::start(dir.path());
        let s = session(&mut c, "writer");
        assert_eq!(s["propose"], json!({"cadence": word}), "{s}");
    }
    // `changes` is in the reply only under cadence `changes`
    let (dir, _s) = configured("propose:\n  cadence: changes\n  changes: 3\n", None);
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "reader");
    assert_eq!(
        s["propose"],
        json!({"cadence": "changes", "changes": 3}),
        "{s}"
    );
    drop(c);
    let (dir, _s) = configured("propose:\n  cadence: task\n  changes: 3\n", None);
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "writer");
    assert_eq!(s["propose"], json!({"cadence": "task"}), "{s}");
    drop(c);
    // the default count is 10
    let (dir, _s) = configured("propose:\n  cadence: changes\n", None);
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "writer");
    assert_eq!(
        s["propose"],
        json!({"cadence": "changes", "changes": 10}),
        "{s}"
    );
}

#[test]
fn di11_a_persons_cadence_overrides_the_projects() {
    let (dir, _s) = configured(
        "propose:\n  cadence: task\n",
        Some("actor: human:qa\npropose:\n  cadence: asked\n"),
    );
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "writer");
    assert_eq!(s["propose"], json!({"cadence": "asked"}), "{s}");
    assert!(s.get("warnings").is_none(), "the key is valid here: {s}");
    drop(c);
    // the count too
    let (dir, _s) = configured(
        "propose:\n  cadence: changes\n  changes: 4\n",
        Some("actor: human:qa\npropose:\n  changes: 2\n"),
    );
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "writer");
    assert_eq!(
        s["propose"],
        json!({"cadence": "changes", "changes": 2}),
        "{s}"
    );
}

#[test]
fn di11_an_unknown_cadence_warns_and_uses_break() {
    let (dir, _s) = configured("propose:\n  cadence: sometimes\n", None);
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "writer");
    assert_eq!(s["propose"], json!({"cadence": "break"}), "{s}");
    let w = warning(
        &s,
        "propose.cadence must be break, task, changes or asked; using break",
    );
    assert_eq!(w["code"], "W_CONFIG_UNKNOWN", "{w}");
    assert_eq!(
        w["fix"], "set propose.cadence to break, task, changes or asked",
        "{w}"
    );
    drop(c);
    // a wrong value in the person's file wins at its level, as `break`, over the project's word
    let (dir, _s) = configured(
        "propose:\n  cadence: task\n",
        Some("actor: human:qa\npropose:\n  cadence: Always\n"),
    );
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "writer");
    assert_eq!(s["propose"], json!({"cadence": "break"}), "{s}");
    warning(
        &s,
        "propose.cadence must be break, task, changes or asked; using break",
    );
    drop(c);
}

#[test]
fn di11_propose_changes_out_of_range_warns() {
    for bad in ["0", "1001", "5000", "-3"] {
        let (dir, _s) = configured(
            &format!("propose:\n  cadence: changes\n  changes: {bad}\n"),
            None,
        );
        let mut c = McpClient::start(dir.path());
        let s = session(&mut c, "writer");
        assert_eq!(
            s["propose"],
            json!({"cadence": "changes", "changes": 10}),
            "{bad}: {s}"
        );
        let w = warning(&s, "propose.changes must be 1–1000; using 10");
        assert_eq!(w["code"], "W_CONFIG_UNKNOWN", "{w}");
        assert_eq!(
            w["fix"], "set propose.changes to a whole number from 1 to 1000",
            "{w}"
        );
    }
    // a wrong count in the person's file wins at its level, as 10, over the project's count
    let (dir, _s) = configured(
        "propose:\n  cadence: changes\n  changes: 4\n",
        Some("actor: human:qa\npropose:\n  changes: 0\n"),
    );
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "writer");
    assert_eq!(
        s["propose"],
        json!({"cadence": "changes", "changes": 10}),
        "{s}"
    );
    warning(&s, "propose.changes must be 1–1000; using 10");
    drop(c);
    // the ends of the range are values
    for ok in ["1", "1000"] {
        let (dir, _s) = configured(
            &format!("propose:\n  cadence: changes\n  changes: {ok}\n"),
            None,
        );
        let mut c = McpClient::start(dir.path());
        let s = session(&mut c, "writer");
        assert_eq!(s["propose"]["changes"], ok.parse::<u32>().unwrap(), "{s}");
        assert!(s.get("warnings").is_none(), "{s}");
    }
}

#[test]
fn di11_propose_due_appears_once_the_count_is_reached() {
    let (dir, section) = configured("propose:\n  cadence: changes\n  changes: 2\n", None);
    let mut c = McpClient::start(dir.path());
    session(&mut c, "writer");
    let first = write_claim(&mut c, &section, "a");
    assert!(first.get("propose_due").is_none(), "{first}");
    let second = write_claim(&mut c, &section, "b");
    assert_eq!(second["propose_due"], true, "{second}");
    // a proposal carries the changes: they no longer count
    let p = c.call("rdy_propose", json!({"title": "two claims"})).ok();
    assert!(p["ref"].is_string(), "{p}");
    let third = write_claim(&mut c, &section, "c");
    assert!(third.get("propose_due").is_none(), "{third}");
    let fourth = write_claim(&mut c, &section, "d");
    assert_eq!(fourth["propose_due"], true, "{fourth}");
}

#[test]
fn di11_propose_due_never_blocks_a_write() {
    let (dir, section) = configured("propose:\n  cadence: changes\n  changes: 1\n", None);
    let mut c = McpClient::start(dir.path());
    session(&mut c, "writer");
    for body in ["a", "b", "c"] {
        let w = write_claim(&mut c, &section, body);
        assert_eq!(w["propose_due"], true, "{w}");
        assert!(w["change"].is_string(), "the write stands: {w}");
    }
    drop(c);
    // under any other cadence the key is absent however many changes wait
    for cadence in ["break", "task", "asked"] {
        let (dir, section) = configured(
            &format!("propose:\n  cadence: {cadence}\n  changes: 1\n"),
            None,
        );
        let mut c = McpClient::start(dir.path());
        session(&mut c, "writer");
        for body in ["a", "b"] {
            let w = write_claim(&mut c, &section, body);
            assert!(w.get("propose_due").is_none(), "{cadence}: {w}");
            assert!(w["change"].is_string(), "{w}");
        }
    }
}

#[test]
fn di11_rdy_session_counts_what_waits() {
    let (dir, r, batch) = batch_project();
    let queue = rdy_qa::intake::pending(&r);
    let items = queue.items.len() + queue.omitted;
    let blocking = queue.items.iter().filter(|i| i.blocking).count();
    assert!(
        items >= 3,
        "the batch, its exceptions and the advice: {queue:?}"
    );
    drop(r);
    append(dir.path(), ".rdy/local.yaml", "actor: human:qa\n");
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "reader");
    assert_eq!(
        s["pending"],
        json!({"items": items, "blocking": blocking}),
        "{} waits: {s}",
        batch.r#ref
    );
    drop(c);
    // a project with nothing waiting says so
    let (dir, _s) = configured("", None);
    let mut c = McpClient::start(dir.path());
    let s = session(&mut c, "writer");
    assert_eq!(s["pending"], json!({"items": 0, "blocking": 0}), "{s}");
}

#[test]
fn di11_the_approver_refusal_names_rdy_answer() {
    let (dir, _s) = configured("", None);
    let mut c = McpClient::start(dir.path());
    let e = c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "w", "role": "approver"}),
    );
    assert_eq!(e.code(), Some("E_PERMISSION"), "{e:?}");
    let text = format!("{e:?}");
    assert!(
        text.contains("approver sessions are not available over MCP"),
        "{text}"
    );
    assert!(
        text.contains("relay the person's answer with rdy_answer"),
        "{text}"
    );
    assert!(!text.contains("rdy approve"), "{text}");
}
