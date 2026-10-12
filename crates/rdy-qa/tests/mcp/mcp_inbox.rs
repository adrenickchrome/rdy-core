//! ib7: `rdy_inbox` over the real server, through the daemon, and the daemon's `inbox` op.
use rdy_qa::daemon::{Hosted, Served};
use rdy_qa::mcp::{McpClient, Reply};
use rdy_qa::packs::{QA_PACK_YAML, use_pack};
use serde_json::{Value, json};

/// A served project whose pack declares the `payments` area.
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

fn agent(s: &Served, areas: Option<Value>) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    let mut args = json!({"actor": "agent:a", "run": "a", "role": "writer"});
    if let Some(a) = areas {
        args["areas"] = a;
    }
    c.call("rdy_session", args).ok();
    c
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

fn kinds(v: &Value) -> Vec<&str> {
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect()
}

#[test]
fn ib7_rdy_inbox_lists_a_landing_in_your_areas() {
    let s = served();
    let mut a = agent(&s, Some(json!(["payments"])));
    land(&s, "payments/fees");
    let first = a.call("rdy_inbox", json!({})).ok();
    assert_eq!(kinds(&first), vec!["landed"], "{first}");
    let acked = a.call("rdy_inbox", json!({"ack": true})).ok();
    assert_eq!(kinds(&acked), vec!["landed"], "{acked}");
    assert_eq!(acked["acked"], true, "{acked}");
    let third = a.call("rdy_inbox", json!({})).ok();
    assert_eq!(third["entries"], json!([]), "{third}");
}

#[test]
fn ib7_an_agent_reads_and_acks_the_persons_inbox_from_chat() {
    let s = served();
    let mut a = agent(&s, Some(json!(["payments"])));
    let started = a.call("rdy_inbox", json!({"person": true})).ok();
    assert_eq!(started["started"], true, "{started}");
    assert_eq!(started["person"], "human:qa", "{started}");
    // the person has an in-flight change in payments; another agent lands one there
    let (ok, v) = s.json(&["--json", "new", "doc", "payments/h"]);
    assert!(ok, "{v}");
    let (ok, v) = s.json_as("agent:b", None, &["--json", "new", "doc", "payments/b"]);
    assert!(ok, "{v}");
    let (ok, p) = s.json_as("agent:b", None, &["--json", "propose", "--title", "t"]);
    assert!(ok, "{p}");
    let (ok, v) = s.json(&["--json", "approve", p["id"].as_str().unwrap()]);
    assert!(ok, "{v}");
    let read = a
        .call("rdy_inbox", json!({"person": true, "ack": true}))
        .ok();
    assert_eq!(kinds(&read), vec!["landed"], "{read}");
    assert_eq!(read["acked"], true, "{read}");
    assert_eq!(read["person"], "human:qa", "{read}");
    // the cursor is the person's, saved: a second read starts past the page, and answers the
    // cursor `rdy inbox` (the CLI twin, Task 9) reads from the same ref
    let again = a.call("rdy_inbox", json!({"person": true})).ok();
    assert_eq!(again["entries"], json!([]), "{again}");
    assert_eq!(again["cursor"], read["cursor"], "{again} {read}");
    assert_eq!(
        again["person"], "human:qa",
        "the reply says whose inbox it is"
    );
    // the agent's own inbox is untouched: the person's ack did not move its cursor, so the
    // landing in its area is still there
    let own = a.call("rdy_inbox", json!({})).ok();
    assert_eq!(kinds(&own), vec!["landed"], "{own}");
    assert!(own.get("started").is_none(), "{own}");
    assert!(own.get("person").is_none(), "{own}");
}

#[test]
fn ib7_rdy_inbox_needs_a_session() {
    let s = served();
    let mut c = McpClient::start_proxy(&s);
    let r = c.call("rdy_inbox", json!({}));
    assert_eq!(r.code(), Some("E_NO_SESSION"), "{r:?}");
    let r = c.call("rdy_inbox", json!({"person": true}));
    assert_eq!(r.code(), Some("E_NO_SESSION"), "{r:?}");
}

#[test]
fn ib7_unknown_keys_are_refused() {
    let s = served();
    let mut a = agent(&s, None);
    let r = a.call("rdy_inbox", json!({"cursor": "x"}));
    assert_eq!(r.code(), Some("E_BAD_INPUT"), "{r:?}");
    let Reply::ToolError(e) = &r else {
        panic!("{r:?}")
    };
    assert!(e["message"].as_str().unwrap().contains("cursor"), "{e}");
}

#[test]
fn ib7_the_daemon_op_serves_an_agent_from_rdy_actor() {
    let h = Hosted::new();
    Hosted::rt().block_on(async {
        let mut a = h.client(Some("agent:a"), Some("a")).await;
        let first = a.ctl("inbox", json!({})).await.unwrap();
        assert_eq!(first["started"], true, "{first}");
        let second = a.ctl("inbox", json!({})).await.unwrap();
        assert_eq!(second["entries"], json!([]), "{second}");
        assert!(second.get("started").is_none(), "{second}");
        let big = a.ctl("inbox", json!({"max_tokens": 1_000_000})).await;
        assert_eq!(big.unwrap_err().code, "E_LIMIT");
    });
}
