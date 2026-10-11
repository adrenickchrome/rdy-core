//! se9: rdy_review_next and rdy_review_submit over the real server, through the daemon.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::mcp::McpClient;
use rdy_qa::tensions::{put, upd};
use serde_json::json;
use std::time::{Duration, Instant};

fn seeded() -> (Served, Spec) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    put(&mut r, "a", upd(&spec.c, "C contradicts B."));
    r.close().unwrap();
    (s, spec)
}
fn client(s: &Served, actor: &str, role: &str) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": actor, "run": actor.replace(':', "-"), "role": role}),
    )
    .ok();
    c
}

#[test]
fn se9_two_reviewers_get_different_requests_and_a_closed_session_frees_its_lease() {
    let (s, _sp) = seeded();
    let mut r1 = client(&s, "agent:r1", "reviewer");
    let mut r2 = client(&s, "agent:r2", "reviewer");
    let a = r1.call("rdy_review_next", json!({"kind": "tension"})).ok();
    let b = r2.call("rdy_review_next", json!({"kind": "tension"})).ok();
    assert_ne!(a["request"], b["request"]);
    // C pairs with B, D and A: r3 takes the third, and then nothing is left
    let mut r3 = client(&s, "agent:r3", "reviewer");
    let c = r3.call("rdy_review_next", json!({"kind": "tension"})).ok();
    assert!(
        c["request"].is_string() && c["request"] != a["request"] && c["request"] != b["request"]
    );
    assert_eq!(
        r3.call("rdy_review_next", json!({"kind": "tension"})).ok()["request"],
        json!(null)
    );
    drop(r1);
    // the daemon frees r1's lease when its session ends
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let n = r3.call("rdy_review_next", json!({"kind": "tension"})).ok();
        if n["request"] == a["request"] {
            break;
        }
        assert!(Instant::now() < deadline, "the lease was never freed: {n}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn se9_a_tension_verdict_shows_in_rdy_tensions_as_advisory_semantic() {
    let (s, _sp) = seeded();
    let mut r = client(&s, "agent:r", "reviewer");
    let n = r.call("rdy_review_next", json!({"kind": "tension"})).ok();
    let block = n["case"]["blocks"][0]["id"].clone();
    let sub = r
        .call(
            "rdy_review_submit",
            json!({
                "request": n["request"], "case_hash": n["case_hash"], "verdict": "tension",
                "findings": [{"block": block, "note": "these two cannot both hold"}]
            }),
        )
        .ok();
    assert_eq!(sub["severity"], "advisory");
    let l = r.call("rdy_tensions", json!({"kind": "semantic"})).ok();
    assert_eq!(l["tensions"][0]["id"], sub["tension"]);
    assert_eq!(l["tensions"][0]["judged"]["actor"], "agent:r");
    let again = r.call(
        "rdy_review_submit",
        json!({
            "request": n["request"], "case_hash": n["case_hash"], "verdict": "no_tension"
        }),
    );
    assert_eq!(again.code(), Some("E_REVIEW_STALE"));
}

#[test]
fn se9_a_writer_session_cannot_take_or_judge_requests() {
    let (s, _sp) = seeded();
    let mut w = client(&s, "agent:w", "writer");
    assert_eq!(
        w.call("rdy_review_next", json!({"kind": "tension"})).code(),
        Some("E_PERMISSION")
    );
    assert_eq!(
        w.call(
            "rdy_review_submit",
            json!({"request": "rq-000000", "case_hash": "0".repeat(64), "verdict": "tension"})
        )
        .code(),
        Some("E_PERMISSION")
    );
}

#[test]
fn se9_advice_then_a_relayed_acceptance_over_mcp() {
    let (s, _sp) = seeded();
    let mut r = client(&s, "agent:r", "reviewer");
    let n = r.call("rdy_review_next", json!({"kind": "tension"})).ok();
    let sub = r
        .call(
            "rdy_review_submit",
            json!({
                "request": n["request"], "case_hash": n["case_hash"], "verdict": "tension",
                "findings": [{"block": n["case"]["blocks"][0]["id"], "note": "these two cannot both hold"}],
                "severity": "blocking", "reason": "it contradicts the gate rule"
            }),
        )
        .ok();
    assert_eq!(
        (sub["severity"].as_str(), sub["advice"].as_str()),
        (Some("advisory"), Some("it contradicts the gate rule"))
    );
    // the writer's agent proposes, shows the person the advice, and relays the answer
    // view `a` is owned by agent:a, run a (the seeding write's session)
    let mut w = McpClient::start_proxy(&s);
    w.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    let p = w
        .call("rdy_propose", json!({"view": "a", "title": "C"}))
        .ok();
    let pref = p["ref"].as_str().unwrap().to_string();
    let digest = p["advised_digest"].as_str().unwrap().to_string();
    let listed = p["tensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == sub["tension"])
        .unwrap()
        .clone();
    assert_eq!(listed["judged"]["advice"], "it contradicts the gate rule");
    let e = w.call(
        "rdy_resolve",
        json!({"accept": {"proposal": pref, "advised_digest": digest}}),
    );
    assert_eq!(
        e.code(),
        Some("E_PERMISSION"),
        "an agent relays the person's words"
    );
    let e = w.call(
        "rdy_resolve",
        json!({"accept": {"proposal": pref, "advised_digest": digest, "evidence": "yes", "relayed_by": "agent:a"}}),
    );
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "no relayed_by over MCP");
    let a = w
        .call(
            "rdy_resolve",
            json!({"accept": {"proposal": pref, "advised_digest": digest, "evidence": "yes, accept it",
        "question": "Accept the reviewer's advice to block?", "session_ref": "chat-1"}}),
        )
        .ok();
    assert_eq!(a["accepted"][0]["was"], sub["tension"]);
    let l = w.call("rdy_tensions", json!({"kind": "semantic"})).ok();
    assert_eq!(l["tensions"][0]["severity"], "blocking");
    assert_eq!(
        l["tensions"][0]["judged"]["accepted"]["evidence"],
        "yes, accept it"
    );
}

#[test]
fn se9_rdy_schema_has_the_reviews_topic() {
    let (s, _sp) = seeded();
    let mut r = client(&s, "agent:r", "reviewer");
    let schema = r.call("rdy_schema", json!({})).ok();
    let doc = schema["reviews"].as_str().unwrap();
    assert!(
        doc.contains("rdy_review_next {kind: tension}") && doc.contains("E_REVIEW_STALE"),
        "{doc}"
    );
}
