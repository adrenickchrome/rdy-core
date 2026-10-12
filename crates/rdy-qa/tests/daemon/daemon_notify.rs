//! ib11: the daemon tells subscribed MCP sessions what changed (spec §10.3; D-KTW43, D-Z0EMP):
//! `rdy://inbox` when a batch gives the session an inbox entry, `rdy://view/{name}` when an event
//! names the view, `rdy://pack` on a pack reload. Notifications are best effort: every wait polls
//! with a deadline, and no test asserts that a notification does not arrive (the inbox is the
//! reliable path, so a test of "no match" reads the inbox).
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::mcp::McpClient;
use rdy_qa::packs::{QA_PACK_YAML, use_pack};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

/// A served project with the `rdy-qa` pack (areas `payments`, `identity`, `glossary`).
fn served() -> Served {
    let s = Served::new();
    use_pack(
        s.dir.path(),
        "rdy-qa",
        QA_PACK_YAML,
        "serve:\n  idle_timeout: 60\n",
    );
    s
}

/// `served()` with the spec doc landed in canon (claims A, B, C), written before any daemon runs.
fn served_spec() -> (Served, Spec) {
    let s = served();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    r.close().unwrap();
    (s, spec)
}

/// Agent `agent:<name>` with run `<name>` (a writer) through the proxy, and its view.
fn agent(s: &Served, name: &str, areas: Option<Value>) -> (McpClient, String) {
    let mut c = McpClient::start_proxy(s);
    let mut args = json!({"actor": format!("agent:{name}"), "run": name, "role": "writer"});
    if let Some(a) = areas {
        args["areas"] = a;
    }
    let v = c.call("rdy_session", args).ok();
    let view = v["view"].as_str().unwrap().to_string();
    (c, view)
}

fn subscribe(c: &mut McpClient, uri: &str) {
    let r = c.subscribe(uri);
    assert!(r.get("error").is_none(), "{uri}: {r}");
}

/// Rewrites block `id` in the session's view from canon's version.
fn update(c: &mut McpClient, id: &rdy_model::BlockId, body: &str) {
    let read = c
        .call(
            "rdy_context",
            json!({"id": id.to_string(), "scope": "canon"}),
        )
        .ok();
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "update", "id": id.to_string(), "base": read["version"], "body": body}]}),
        )
        .ok();
    assert!(w["change"].is_string(), "{w}");
}

/// The person lands a new doc at `path` through the CLI: create, propose, approve.
fn land(s: &Served, path: &str) {
    let (ok, v) = s.json(&["--json", "new", "doc", path]);
    assert!(ok, "{v}");
    let (ok, p) = s.json(&["--json", "propose", "--title", "t"]);
    assert!(ok, "{p}");
    let (ok, v) = s.json(&["--json", "approve", p["id"].as_str().unwrap()]);
    assert!(ok, "{v}");
}

fn entries(v: &Value) -> Vec<Value> {
    v["entries"].as_array().unwrap().clone()
}

fn kinds(v: &Value) -> Vec<String> {
    entries(v)
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect()
}

/// How many `notifications/resources/updated` for `uri` the client holds.
fn updates(c: &McpClient, uri: &str) -> usize {
    c.notifications()
        .iter()
        .filter(|n| n["method"] == "notifications/resources/updated" && n["params"]["uri"] == uri)
        .count()
}

/// `wait_notification` for a later update: true once the client holds more than `seen` updates
/// for `uri`, polling for at most `budget`. A wait after an earlier update of the same URI uses
/// it, so the earlier update never answers it.
fn wait_another(c: &mut McpClient, uri: &str, seen: usize, budget: Duration) -> bool {
    let end = Instant::now() + budget;
    loop {
        if updates(c, uri) > seen {
            return true;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        c.drain(left.min(Duration::from_millis(50)));
    }
}

/// Agent `a` changes C; it subscribes to its inbox; agent `b` changes C otherwise (an edit
/// collision on `a`'s change). Returns both agents, `b`'s view and the spec.
fn tension_on_a(s: &Served, spec: &Spec) -> (McpClient, McpClient, String) {
    let (mut a, _) = agent(s, "a", None);
    update(&mut a, &spec.c, "C, by a.");
    subscribe(&mut a, "rdy://inbox");
    let (mut b, vb) = agent(s, "b", None);
    update(&mut b, &spec.c, "C, by b.");
    assert!(
        a.wait_notification("rdy://inbox", WAIT),
        "a heard no rdy://inbox update: {:?}",
        a.notifications()
    );
    (a, b, vb)
}

#[test]
fn ib11_a_tension_on_your_change_notifies_your_inbox() {
    let (s, spec) = served_spec();
    let (mut a, _b, _) = tension_on_a(&s, &spec);
    let page = a.call("rdy_inbox", json!({})).ok();
    assert!(kinds(&page).iter().any(|k| k == "tension_opened"), "{page}");
}

#[test]
fn ib11_a_resolution_on_your_change_notifies_your_inbox() {
    let (s, spec) = served_spec();
    let (mut a, _b, vb) = tension_on_a(&s, &spec);
    let acked = a.call("rdy_inbox", json!({"ack": true})).ok();
    assert_eq!(acked["acked"], true, "{acked}");
    let seen = updates(&a, "rdy://inbox");
    let (ok, v) = s.json(&["--json", "view", "close", &vb, "--reason", "by hand"]);
    assert!(ok, "{v}");
    assert!(
        wait_another(&mut a, "rdy://inbox", seen, WAIT),
        "a heard no rdy://inbox update for the resolution: {:?}",
        a.notifications()
    );
    let page = a.call("rdy_inbox", json!({})).ok();
    let got = entries(&page);
    assert_eq!(kinds(&page), vec!["tension_resolved"], "{page}");
    // the line ends with its cause: the close of view b, the op its ids name
    let ids = got[0]["ids"].as_array().unwrap();
    assert_eq!(ids.len(), 2, "{page}");
    let op = ids[1].as_str().unwrap();
    let line = got[0]["line"].as_str().unwrap();
    assert!(
        line.ends_with(&format!(
            "no longer involves your in-flight changes: a view changed (op {})",
            &op[..12]
        )),
        "{line}"
    );
}

#[test]
fn ib11_a_landing_in_your_area_notifies_your_inbox() {
    let s = served();
    let (mut a, _) = agent(&s, "a", Some(json!(["payments"])));
    subscribe(&mut a, "rdy://inbox");
    land(&s, "payments/fees");
    assert!(
        a.wait_notification("rdy://inbox", WAIT),
        "a heard no rdy://inbox update: {:?}",
        a.notifications()
    );
    let page = a.call("rdy_inbox", json!({})).ok();
    assert_eq!(kinds(&page), vec!["landed"], "{page}");
}

#[test]
fn ib11_a_landing_outside_your_areas_does_not_match() {
    let s = served();
    let (mut a, _) = agent(&s, "a", Some(json!(["payments"])));
    subscribe(&mut a, "rdy://inbox");
    land(&s, "identity/login");
    land(&s, "payments/fees");
    assert!(
        a.wait_notification("rdy://inbox", WAIT),
        "a heard no rdy://inbox update: {:?}",
        a.notifications()
    );
    // the inbox, not the absence of a notification, says the identity landing did not match
    let page = a.call("rdy_inbox", json!({})).ok();
    let got = entries(&page);
    assert!(!got.is_empty(), "{page}");
    assert_eq!(got[0]["event"], "landed", "{page}");
    assert!(
        got[0]["line"].as_str().unwrap().ends_with("(payments)"),
        "{page}"
    );
    assert!(
        got.iter().all(|e| !e.to_string().contains("identity")),
        "{page}"
    );
}

#[test]
fn ib11_a_subscribed_view_notifies_on_a_commit() {
    let s = served();
    let (mut a, _) = agent(&s, "a", None);
    let (mut b, vb) = agent(&s, "b", None);
    let uri = format!("rdy://view/{vb}");
    subscribe(&mut a, &uri);
    let w = b
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "doc",
                "fields": {"name": "n", "title": "N", "doc_type": "glossary"}}]}),
        )
        .ok();
    assert!(w["change"].is_string(), "{w}");
    assert!(
        a.wait_notification(&uri, WAIT),
        "a heard no {uri} update: {:?}",
        a.notifications()
    );
}

#[test]
fn ib11_a_pack_reload_notifies_rdy_pack() {
    let s = served();
    let (mut a, _) = agent(&s, "a", None);
    subscribe(&mut a, "rdy://pack");
    let edited = QA_PACK_YAML.replace("money in and out", "money in and out, edited");
    assert_ne!(edited, QA_PACK_YAML);
    std::fs::write(s.dir.path().join(".rdy/packs/rdy-qa.yaml"), edited).unwrap();
    // a new ctl connection runs the reload check before its first reply
    let out = s.rdy(&["status"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        a.wait_notification("rdy://pack", WAIT),
        "a heard no rdy://pack update: {:?}",
        a.notifications()
    );
    // the reply notice of D-AHYBK stays as it is
    let v = a.call("rdy_overview", json!({})).ok();
    assert!(
        v["notice"]
            .as_str()
            .is_some_and(|n| n.starts_with("pack_changed")),
        "{}",
        v["notice"]
    );
}

/// A guard that the daemon keeps serving after a subscribed session goes. It cannot see a dead
/// session left in the hub (a send to it fails silently): rdy-daemon's
/// `ib11_a_session_end_removes_it_from_the_hub` pins the session-end hook itself.
#[test]
fn ib11_a_closed_session_leaves_the_hub() {
    let s = served();
    let (mut b, vb) = agent(&s, "b", None);
    let uri = format!("rdy://view/{vb}");
    let (mut a, _) = agent(&s, "a", None);
    subscribe(&mut a, &uri);
    subscribe(&mut a, "rdy://inbox");
    drop(a);
    subscribe(&mut b, &uri);
    let w = b
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "doc",
                "fields": {"name": "n", "title": "N", "doc_type": "glossary"}}]}),
        )
        .ok();
    assert!(w["change"].is_string(), "{w}");
    // the notifier still runs: b hears of its own view
    assert!(
        b.wait_notification(&uri, WAIT),
        "b heard no {uri} update: {:?}",
        b.notifications()
    );
    let log = std::fs::read_to_string(s.run_dir().log_file()).unwrap_or_default();
    assert!(
        log.lines().all(|l| !l.contains("notify")),
        "the daemon logged a notify error: {log}"
    );
}

/// Agent `b` writes doc `ops` (path `ops`) with a claim in its view. Returns the ids of the doc
/// and the claim.
fn ops_doc(b: &mut McpClient) -> (String, String) {
    let w = b
        .call(
            "rdy_write",
            json!({"ops": [
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "glossary"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
                {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Ops holds."}
            ]}),
        )
        .ok();
    let id = |k: &str| w["created"][k].as_str().unwrap().to_string();
    (id("$d"), id("$c"))
}

#[test]
fn ib11_a_doc_subscription_by_id_path_or_claim_hears_its_changes() {
    let s = served();
    let (mut b, vb) = agent(&s, "b", None);
    let (doc, claim) = ops_doc(&mut b);
    let uris = [
        format!("rdy://doc/{doc}"),
        "rdy://doc/ops".to_string(),
        format!("rdy://doc/{claim}"),
        format!("rdy://view/{vb}"),
    ];
    for uri in &uris {
        subscribe(&mut b, uri);
        let read = b.read_resource(uri);
        assert!(read.get("error").is_none(), "{uri}: {read}");
    }
    let w = b
        .call(
            "rdy_write",
            json!({"ops": [{"op": "update", "id": claim, "body": "Ops holds, still."}]}),
        )
        .ok();
    assert!(w["change"].is_string(), "{w}");
    // the doc by id and by path hear of a change in it; the claim's URI hears of the claim
    for uri in &uris {
        assert!(
            b.wait_notification(uri, WAIT),
            "b heard no {uri} update: {:?}",
            b.notifications()
        );
    }
}

#[test]
fn ib11_an_unresolvable_doc_uri_is_refused_as_its_read_is() {
    let s = served();
    let (mut b, _) = agent(&s, "b", None);
    for uri in ["rdy://doc/no/such/doc", "rdy://doc/c-ZZZZZ"] {
        let sub = b.subscribe(uri);
        let read = b.read_resource(uri);
        assert!(sub.get("error").is_some(), "{uri}: {sub}");
        assert!(read.get("error").is_some(), "{uri}: {read}");
        assert_eq!(
            sub["error"]["code"], read["error"]["code"],
            "{uri}: {sub} {read}"
        );
        assert_eq!(sub["error"]["message"], read["error"]["message"], "{uri}");
    }
}
