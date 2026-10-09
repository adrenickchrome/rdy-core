//! mten1: rdy_tensions and rdy_resolve over the real server, through the daemon.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::land;
use rdy_qa::mcp::McpClient;
use rdy_qa::tensions::{put, term};
use serde_json::json;

fn seeded() -> (Served, Spec) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    r.close().unwrap();
    (s, spec)
}
fn client(s: &Served, actor: &str, run: &str, role: &str) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": role}),
    )
    .ok();
    c
}

#[test]
fn mten1_tensions_are_listed_filtered_and_dismissed_by_the_rules() {
    let (s, sp) = seeded();
    let mut a = client(&s, "agent:a", "a", "writer");
    let mut b = client(&s, "agent:b", "b", "writer");
    b.call(
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "claim", "parent": sp.section.to_string(),
        "body": format!("Builds on {{{{{}}}}}.", sp.a)}]}),
    )
    .ok();
    let wa = a
        .call(
            "rdy_write",
            json!({"ops": [{"op": "update", "id": sp.a.to_string(), "body": "A changed."}]}),
        )
        .ok();
    assert_eq!(wa["on_change"]["tensions"][0]["kind"], "premise_shift");
    let all = a.call("rdy_tensions", json!({})).ok();
    assert_eq!(all["tensions"].as_array().unwrap().len(), 1);
    assert_eq!(all["counts"]["blocking_open"], 1);
    let tn = all["tensions"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        a.call("rdy_tensions", json!({"kind": "fork"})).ok()["tensions"],
        json!([])
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"involving": sp.a.to_string()}))
            .ok()["tensions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"view": "b", "severity": "blocking"}))
            .ok()["tensions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"scope": "canon"})).ok()["tensions"],
        json!([]),
        "the seed has no fork, duplicate or deprecated content: no all-canon tension"
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"status": "resolved"})).code(),
        Some("E_BAD_INPUT")
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"kind": "clash"})).code(),
        Some("E_BAD_INPUT")
    );
    assert_eq!(
        a.call(
            "rdy_resolve",
            json!({"tension": tn, "dismiss": {"reason": "fine"}})
        )
        .code(),
        Some("E_PERMISSION")
    );
    let mut same = client(&s, "agent:a", "review", "reviewer");
    assert_eq!(
        same.call(
            "rdy_resolve",
            json!({"tension": tn, "dismiss": {"reason": "fine"}})
        )
        .code(),
        Some("E_SELF_REVIEW")
    );
    let mut rv = client(&s, "agent:r", "r", "reviewer");
    let ok = rv
        .call(
            "rdy_resolve",
            json!({"tension": tn, "dismiss": {"reason": "B still holds"}}),
        )
        .ok();
    assert_eq!(ok["status"], "dismissed");
    assert_eq!(
        a.call("rdy_tensions", json!({})).ok()["tensions"],
        json!([]),
        "the default lists open ones"
    );
    let d = a.call("rdy_tensions", json!({"status": "dismissed"})).ok();
    assert_eq!(d["tensions"][0]["dismissal"]["reason"], "B still holds");
    assert_eq!(d["tensions"][0]["dismissal"]["role"], "reviewer");
    let reviews = a
        .call(
            "rdy_tensions",
            json!({"status": "dismissed", "reviews": true}),
        )
        .ok();
    assert_eq!(reviews["reviews"][0]["holds"], true);
    assert_eq!(reviews["reviews"][0]["actor"], "agent:r");
    assert_eq!(
        a.call("rdy_resolve", json!({"tension": tn})).code(),
        Some("E_BAD_INPUT")
    );
    let schema = a.call("rdy_schema", json!({})).ok();
    for code in ["E_BLOCKING_TENSION", "E_SELF_REVIEW"] {
        assert!(schema.to_string().contains(code), "{code}");
    }
    assert!(schema["tensions"].as_str().unwrap().contains("rdy_resolve"));
}

#[test]
fn mten1_scope_filters_and_the_cut() {
    let (s, sp) = seeded();
    let mut a = client(&s, "agent:a", "a", "writer");
    let mut b = client(&s, "agent:b", "b", "writer");
    b.call(
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "claim", "parent": sp.section.to_string(),
        "body": format!("Builds on {{{{{}}}}}.", sp.a)}]}),
    )
    .ok();
    a.call(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": sp.a.to_string(), "body": "A changed."}]}),
    )
    .ok();
    let n = |c: &mut McpClient, args: serde_json::Value| {
        c.call("rdy_tensions", args).ok()["tensions"]
            .as_array()
            .unwrap()
            .len()
    };
    // a scope that is a view is the view filter; each side's view lists the tension
    assert_eq!(n(&mut a, json!({"scope": "a"})), 1);
    assert_eq!(n(&mut a, json!({"scope": "b"})), 1);
    assert_eq!(
        n(&mut a, json!({"scope": "inflight", "view": "work"})),
        0,
        "no change of work is a side"
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"scope": "nope"})).code(),
        Some("E_NOT_FOUND")
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"view": "nope"})).code(),
        Some("E_NOT_FOUND")
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"scope": "a", "view": "b"}))
            .code(),
        Some("E_BAD_INPUT")
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"severity": "huge"})).code(),
        Some("E_BAD_INPUT")
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"status": "all"})).code(),
        Some("E_BAD_INPUT")
    );
    assert_eq!(
        a.call("rdy_tensions", json!({"area": "nowhere"})).code(),
        Some("E_BAD_INPUT")
    );
    // a change by hex prefix, and a block by id
    let all = a.call("rdy_tensions", json!({})).ok();
    let change = all["tensions"][0]["sides"][0]["change"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(n(&mut a, json!({"involving": &change[..10]})), 1);
    assert_eq!(n(&mut a, json!({"involving": sp.c.to_string()})), 0);
    // a cut keeps the counts and says what it left out
    let cut = a.call("rdy_tensions", json!({"max_tokens": 1})).ok();
    assert_eq!(cut["tensions"], json!([]));
    assert_eq!(cut["omitted"], 1);
    assert_eq!(cut["counts"]["open"], 1);
    // a resolve of no tension, and a reason of blanks
    assert_eq!(
        a.call(
            "rdy_resolve",
            json!({"tension": "tn-000000000000", "dismiss": {"reason": "x"}})
        )
        .code(),
        Some("E_NOT_FOUND")
    );
    let mut rv = client(&s, "agent:r", "r", "reviewer");
    assert_eq!(
        rv.call(
            "rdy_resolve",
            json!({"tension": all["tensions"][0]["id"], "dismiss": {"reason": "  "}})
        )
        .code(),
        Some("E_BAD_INPUT")
    );
    let reviews = a.call("rdy_tensions", json!({"reviews": true})).ok();
    assert_eq!(reviews["reviews"], json!([]), "no dismissal yet");
}

#[test]
fn mten1_scope_canon_lists_only_tensions_whose_sides_are_all_canon() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let sp = spec_doc(&mut r);
    put(&mut r, "a", term(&sp.section, "Fee"));
    put(&mut r, "b", term(&sp.section, "fee"));
    land(&mut r, "a").unwrap();
    r.close().unwrap();
    let mut c = client(&s, "agent:c", "c", "writer");
    let kinds = |c: &mut McpClient, scope: &str| -> Vec<String> {
        c.call("rdy_tensions", json!({"scope": scope})).ok()["tensions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["kind"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(kinds(&mut c, "inflight"), ["duplicate_definition"]);
    assert!(
        kinds(&mut c, "canon").is_empty(),
        "b is still in flight: a side is a change"
    );
    drop(c);
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    land(&mut r, "b").unwrap();
    r.close().unwrap();
    let mut c = client(&s, "agent:c", "c", "writer");
    assert_eq!(kinds(&mut c, "canon"), ["duplicate_definition"]);
    assert_eq!(
        kinds(&mut c, "inflight"),
        ["duplicate_definition"],
        "inflight lists every tension"
    );
    let t = &c.call("rdy_tensions", json!({"scope": "canon"})).ok()["tensions"][0];
    assert!(
        t["sides"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["side"].as_str().unwrap().starts_with("canon")),
        "{t}"
    );
}
