//! The site is for people; agents read through tools (FW-V1, FW-D5 refined). What the removed
//! page JSON twin and the "For agents" panel gave an agent, the MCP read tools give (people1).
use rdy_engine::Scope;
use rdy_mcp::tools::*;
use rdy_qa::golden::*;
use serde_json::{Value, json};

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn has_edge(edges: &Value, end: &str, id: &str, kind: &str) -> bool {
    edges
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e[end] == id && e["kind"]["Link"] == kind)
}

#[test]
fn people1_the_read_tools_give_an_agent_what_the_twin_gave() {
    let (_d, mut r, ids) = decided_site();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    // the reader's bridge: the line Copy ref puts on the clipboard
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = html(out.path(), "ledgers.html");
    let at = h
        .find(&format!("data-rdy-block=\"{}\"", ids.requirement))
        .unwrap();
    let copied = rdy_qa::site::unescape(rdy_qa::site::attrs(&h[at..], "data-ref")[0]);
    let project = (r.read().project_name(), r.read().project_description());
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:qa", "role": "reader", "view": "canon"})),
    )
    .unwrap();

    // a block: id, type, doc, parent, fields, suspects, governing (live) and every governing edge
    let c = rdy_context(&mut st, p(json!({"id": copied}))).unwrap();
    assert_eq!(c["id"], ids.requirement.to_string(), "{c}");
    assert_eq!(c["block_type"], "requirement");
    assert_eq!(c["doc"], ids.doc.to_string());
    assert!(
        c["parent"].is_string() && c["fields"].is_object() && c["suspects"].is_array(),
        "{c}"
    );
    assert_eq!(
        c["governing"],
        json!([ids.d1.to_string()]),
        "live governing decisions"
    );
    for d in [&ids.d0, &ids.d1, &ids.d2] {
        assert!(
            has_edge(&c["incoming"], "from", &d.to_string(), "affects"),
            "{d} governs the block through affects: {c}"
        );
    }

    // a decision: statement, decided_at, options, status, supersedes
    let d1 = rdy_context(&mut st, p(json!({"id": ids.d1.to_string()}))).unwrap();
    assert_eq!(d1["fields"]["statement"], "Ledgers are atomic");
    assert_eq!(d1["fields"]["decided_at"], "2021-02-08T10:00:00Z");
    assert_eq!(d1["fields"]["options"], json!(["atomic", "eventual"]));
    assert_eq!(d1["status"]["status"], "live");
    assert!(
        has_edge(&d1["outgoing"], "to", &ids.d0.to_string(), "supersedes"),
        "{d1}"
    );
    for (d, want) in [
        (&ids.d0, "superseded"),
        (&ids.d2, "rejected"),
        (&ids.d3, "revoked"),
    ] {
        let x = rdy_context(&mut st, p(json!({"id": d.to_string()}))).unwrap();
        assert_eq!(x["status"]["status"], want, "{d}: {x}");
    }

    // a page: every block with its type and summary, a status on each decision
    let read = rdy_read(&mut st, p(json!({"id": "ledgers", "max_tokens": 50000}))).unwrap();
    let text = read["text"].as_str().unwrap();
    assert!(
        text.contains(&format!(
            "{} requirement: Ledgers are atomic when settled.",
            ids.requirement
        )),
        "{text}"
    );
    let d1_line = text
        .lines()
        .find(|l| l.contains(&format!("{} decision: Ledgers are atomic", ids.d1)))
        .unwrap_or_else(|| panic!("no line for {} in {text}", ids.d1));
    assert!(d1_line.ends_with("[live]"), "{d1_line}");

    // the version: its name and its landing hash
    let ov = rdy_overview(&mut st, p(json!({}))).unwrap();
    let v = &ov["version_position"]["version"];
    assert_eq!(v["name"], "v1.0.0", "{ov}");
    assert_eq!(v["landing"].as_str().unwrap().len(), 64);
    // the project: its name and description, as the site shows them
    assert_eq!(ov["project"]["name"], project.0, "{ov}");
    assert_eq!(
        ov["project"]["description"],
        serde_json::to_value(&project.1).unwrap(),
        "{ov}"
    );

    // whether a type is presentational: the pack's traits
    let s = rdy_schema(&mut st, p(json!({}))).unwrap();
    assert!(
        s["types"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["traits"].is_array()),
        "{s}"
    );
}
