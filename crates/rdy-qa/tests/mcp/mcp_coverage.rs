//! cc12: `rdy_coverage` over MCP (FW-CV1, D-KE84J).
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{land, write};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

/// A landed doc with a section and an entity "Ledger service" (alias ledger), served; an agent
/// session (run `a1`, so its view is `a1`). Returns (served, client, section).
fn mcp_project() -> (Served, McpClient, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service", "aliases": ["ledger"]}}
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
    (s, c, w.created["$s"].clone())
}

fn mention_claims(section: &str, n: usize) -> Value {
    let ops: Vec<Value> = (0..n)
        .map(|i| {
            json!({"op": "create", "type": "claim", "parent": section,
                   "body": format!("The ledger is append-only, rule {i}.")})
        })
        .collect();
    json!({ "ops": ops })
}

#[test]
fn cc12_rdy_coverage_over_mcp_defaults_to_the_view() {
    let (_d, mut c, section) = mcp_project();
    let w = c.call("rdy_write", mention_claims(&section, 1)).ok();
    let block = w["suggestions"][0]["block"].as_str().unwrap().to_string();
    let v = c.call("rdy_coverage", json!({})).ok();
    assert_eq!(v["scope"], "view/a1", "{v}");
    // cc17: the report carries the onboarding path (FW-CO4a)
    assert_eq!(v["onboarding"]["target"], 80, "{v}");
    assert_eq!(
        v["onboarding"]["phases"].as_array().unwrap().len(),
        4,
        "{v}"
    );
    assert!(
        v["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| { i["code"] == "unlinked_mention" && i["block"] == block.as_str() }),
        "the unlinked mention in the view is listed: {v}"
    );
    let canon = c.call("rdy_coverage", json!({"scope": "canon"})).ok();
    assert!(
        canon["issues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["block"] != block.as_str()),
        "canon does not have it: {canon}"
    );
    let narrow = c.call("rdy_coverage", json!({"path": "ops"})).ok();
    assert!(narrow["pages"].as_array().is_some(), "{narrow}");
    assert!(!c.call("rdy_coverage", json!({"path": "nowhere"})).is_ok());
}

#[test]
fn cc12_a_small_budget_cuts_issues_and_says_so() {
    let (_d, mut c, section) = mcp_project();
    c.call("rdy_write", mention_claims(&section, 8)).ok();
    let v = c.call("rdy_coverage", json!({"max_tokens": 200})).ok();
    assert_eq!(v["truncated"], true, "{v}");
    assert!(v["omitted"].as_u64().unwrap() > 0, "{v}");
    assert!(v["issues_total"].as_u64().unwrap() >= 8, "{v}");
    assert_eq!(
        v["issues"].as_array().unwrap().len() as u64 + v["omitted"].as_u64().unwrap(),
        v["issues_total"].as_u64().unwrap(),
        "the cut list is the issues: {v}"
    );
}

/// Final review M4 (the task-6 ruling): the tool descriptions state the past-landing rule. A
/// briefing and a search at a past landing leave coverage out (a coverage filter or sort is
/// refused), and the overview at a past landing leaves coverage out.
#[test]
fn cc6_the_tool_descriptions_state_the_past_landing_rule() {
    let (dir, r) = rdy_qa::fixtures::project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let description = |name: &str| -> String {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .and_then(|t| t["description"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    for (tool, says) in [
        ("rdy_brief", "at a past landing it leaves coverage out"),
        (
            "rdy_search",
            "at a past landing hits carry no coverage, and coverage_below or sort is refused",
        ),
        (
            "rdy_overview",
            "at a past landing the overview leaves coverage out",
        ),
    ] {
        let d = description(tool);
        assert!(d.contains(says), "{tool}: {d}");
    }
    c.alive();
}
