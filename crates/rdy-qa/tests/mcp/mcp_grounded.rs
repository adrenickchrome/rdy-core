//! mi1 (D-KVQ7F): only a normative block's unlinked mention scores. The tools and the CLI give
//! one result, and suggestions and mention reviews work as before.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{land, write};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Canon: entities Ledger and Archive; prose `p` describes the Ledger and names it; prose `q`
/// describes the Archive and names the Ledger unlinked; claim `c` describes the Archive and names
/// the Ledger unlinked. Served, with an agent session (view `a1`).
fn project() -> (Served, McpClient, BTreeMap<String, String>) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger", "aliases": ["books"]}},
        {"op": "create", "type": "entity", "id": "$a", "parent": "$s", "fields": {"name": "Archive"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "The ledger closes nightly.",
            "links": {"describes": [{"target": "$e"}]}},
        {"op": "create", "type": "prose", "id": "$q", "parent": "$s", "body": "The ledger is archived weekly.",
            "links": {"describes": [{"target": "$a"}]}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "The ledger is retained yearly.",
            "links": {"describes": [{"target": "$a"}]}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    r.close().unwrap();
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a1", "role": "writer"}),
    )
    .ok();
    (s, c, w.created)
}

fn codes_of(issues: &Value, block: &str) -> Vec<String> {
    issues
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["block"] == block)
        .map(|i| i["code"].as_str().unwrap().to_string())
        .collect()
}

fn entities_phase(v: &Value) -> Value {
    v["onboarding"]["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["phase"] == "entities")
        .unwrap_or_else(|| panic!("an entities phase: {v}"))
        .clone()
}

#[test]
fn mi1_tools_and_cli_share_grounded_results() {
    let (s, mut c, ids) = project();
    let (p, q, cl, e) = (&ids["$p"], &ids["$q"], &ids["$c"], &ids["$e"]);

    // rdy_coverage: only the claim's mention is an issue, with its located words
    let v = c.call("rdy_coverage", json!({"scope": "canon"})).ok();
    assert_eq!(codes_of(&v["issues"], p), Vec::<String>::new(), "{v}");
    assert_eq!(codes_of(&v["issues"], q), Vec::<String>::new(), "{v}");
    assert_eq!(codes_of(&v["issues"], cl), ["unlinked_mention"], "{v}");
    let issue = v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["block"] == cl.as_str())
        .unwrap();
    assert_eq!(
        issue["text"],
        json!({"field": "body", "line": 1, "span": [4, 10], "words": "ledger"}),
        "{v}"
    );
    let phase = entities_phase(&v);
    assert_eq!(
        (phase["issues"].as_u64(), phase["score"].as_u64()),
        (Some(1), Some(75)),
        "{v}"
    );
    assert_eq!(phase["dimension_issues"], json!({"grounded": 1}));

    // rdy_search: the informative hit has no issue; the claim has one
    let mut hit = |word: &str, id: &str| -> Value {
        let r = c
            .call("rdy_search", json!({"query": word, "scope": "canon"}))
            .ok();
        r["matches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap_or_else(|| panic!("{word}: {r}"))
            .clone()
    };
    assert_eq!(hit("weekly", q)["issues"], json!(0));
    assert_eq!(hit("yearly", cl)["issues"], json!(1));

    // rdy_brief: the same issues
    let b = c
        .call("rdy_brief", json!({"ids": [p, q, cl], "scope": "canon"}))
        .ok();
    assert_eq!(
        codes_of(&b["coverage"]["issues"], p),
        Vec::<String>::new(),
        "{b}"
    );
    assert_eq!(
        codes_of(&b["coverage"]["issues"], q),
        Vec::<String>::new(),
        "{b}"
    );
    assert_eq!(
        codes_of(&b["coverage"]["issues"], cl),
        ["unlinked_mention"],
        "{b}"
    );
    let reasons = b["coverage"]["reasons"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(reasons.iter().all(|r| r["block"] != q.as_str()), "{b}");

    // write suggestions: a new prose block that names the Ledger still gets a link suggestion,
    // and the write is not refused
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "prose", "parent": ids["$s"], "body": "Then the ledger runs."}]}),
        )
        .ok();
    let sg = w["suggestions"].as_array().unwrap();
    assert_eq!(sg.len(), 1, "{w}");
    assert_eq!(sg[0]["target"], e.as_str(), "{w}");
    let new_prose = sg[0]["block"].as_str().unwrap().to_string();

    // rdy_resolve: a dismissal of the informative mention changes no score; the claim's is settled
    let before = c.call("rdy_coverage", json!({"scope": "canon"})).ok();
    let dismiss = |c: &mut McpClient, block: &str| {
        c.call(
            "rdy_resolve",
            json!({"mention": {"block": block, "entity": e}, "dismiss": {"reason": "a passing word"}}),
        )
        .ok()
    };
    let restore = |c: &mut McpClient, block: &str| {
        c.call(
            "rdy_resolve",
            json!({"mention": {"block": block, "entity": e}, "restore": {"reason": "it needs a link"}}),
        )
        .ok()
    };
    assert_eq!(dismiss(&mut c, q)["status"], "dismissed");
    let mid = c.call("rdy_coverage", json!({"scope": "canon"})).ok();
    assert_eq!(
        mid["score"], before["score"],
        "an informative dismissal scores nothing"
    );
    assert_eq!(mid["issues"], before["issues"]);
    assert_eq!(restore(&mut c, q)["status"], "open");
    assert_eq!(dismiss(&mut c, cl)["status"], "dismissed");
    let settled = c.call("rdy_coverage", json!({"scope": "canon"})).ok();
    assert_eq!(
        codes_of(&settled["issues"], cl),
        Vec::<String>::new(),
        "{settled}"
    );
    assert_eq!(restore(&mut c, cl)["status"], "open");
    let after = c.call("rdy_coverage", json!({"scope": "canon"})).ok();
    assert_eq!(after["issues"], before["issues"]);
    assert_eq!(after["score"], before["score"]);
    let _ = new_prose;

    // the CLI: the same score, issues and Entities phase, in --json and in text
    let out = s.rdy(&["coverage", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cli: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(cli["score"], after["score"], "{cli}");
    assert_eq!(cli["dimensions"], after["dimensions"], "{cli}");
    assert_eq!(cli["issues"], after["issues"], "{cli}");
    assert_eq!(entities_phase(&cli), phase);
    let text = String::from_utf8_lossy(&s.rdy(&["coverage"]).stdout).into_owned();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("unlinked_mention"))
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].contains(cl.as_str()), "{text}");
    assert!(
        !text.contains(q.as_str()) && !text.contains(p.as_str()),
        "{text}"
    );
    s.stop();
}
