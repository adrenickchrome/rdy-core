use rdy_engine::Rdy;
use rdy_mcp::tools::*;
use serde_json::{Value, json};

fn state() -> (tempfile::TempDir, McpState) {
    let dir = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, McpState::new(rdy))
}

fn start(st: &mut McpState) {
    rdy_session(
        st,
        serde_json::from_value(
            json!({"actor": "agent:designer", "run": "run-7", "role": "writer"}),
        )
        .unwrap(),
    )
    .unwrap();
}

fn seed(st: &mut McpState) -> Value {
    rdy_write(st, serde_json::from_value(json!({"ops": [
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Uses the {{$t}}."}
    ]})).unwrap()).unwrap()
}

#[test]
fn tools_require_a_session_and_refuse_approver() {
    let (_d, mut st) = state();
    let e = rdy_overview(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap_err();
    assert_eq!(e.code, "E_NO_SESSION");
    let e = rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:x", "role": "approver"})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PERMISSION");
    assert!(
        rdy_schema(&mut st, serde_json::from_value(json!({})).unwrap()).is_ok(),
        "schema needs no session"
    );
}

#[test]
fn session_view_defaults_to_run() {
    let (_d, mut st) = state();
    start(&mut st);
    assert_eq!(st.view, "run-7");
    seed(&mut st);
    let ov = rdy_overview(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert_eq!(ov["views"][0]["name"], "run-7");
    assert_eq!(ov["pack"], "rdy-standard");
}

#[test]
fn write_read_context_round_trip() {
    let (_d, mut st) = state();
    start(&mut st);
    let res = seed(&mut st);
    let doc = res["created"]["$doc"].as_str().unwrap().to_string();
    let c = res["created"]["$c"].as_str().unwrap().to_string();
    let t = res["created"]["$t"].as_str().unwrap().to_string();

    let read = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": doc, "scope": "run-7"})).unwrap(),
    )
    .unwrap();
    let text = read["text"].as_str().unwrap();
    assert!(text.contains(&format!("{t} term: Ledger")));
    assert!(
        text.contains(&format!("  {c} claim:")),
        "indented by depth:\n{text}"
    );
    assert!(
        text.contains(&format!("{c} claim: Uses the Ledger.")),
        "references render as the target's summary:\n{text}"
    );

    let md = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": doc, "scope": "run-7", "format": "markdown"})).unwrap(),
    )
    .unwrap();
    assert!(md["text"].as_str().unwrap().contains("# Identity"));

    let ctx = rdy_context(
        &mut st,
        serde_json::from_value(json!({"id": c, "scope": "run-7"})).unwrap(),
    )
    .unwrap();
    assert_eq!(ctx["block_type"], "claim");
    assert_eq!(ctx["doc"], doc);

    let imp = rdy_impact(
        &mut st,
        serde_json::from_value(json!({"id": t, "scope": "run-7"})).unwrap(),
    )
    .unwrap();
    assert_eq!(imp["items"][0]["id"], c);
}

#[test]
fn bad_ops_explain_themselves() {
    let (_d, mut st) = state();
    start(&mut st);
    let e = rdy_write(
        &mut st,
        serde_json::from_value(json!({"ops": [{"op": "create"}]})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.message.contains("type"), "{}", e.message);
    let j = error_json(&e);
    assert!(!j["fix"].as_str().unwrap().is_empty());
}

#[test]
fn decide_and_propose() {
    let (_d, mut st) = state();
    start(&mut st);
    let res = seed(&mut st);
    let s = res["created"]["$s"].as_str().unwrap();
    let d = rdy_decide(
        &mut st,
        serde_json::from_value(json!({"parent": s, "statement": "Ledger is append-only"})).unwrap(),
    )
    .unwrap();
    assert!(
        d["created"]["$decision"]
            .as_str()
            .unwrap()
            .starts_with("D-")
    );
    let p = rdy_propose(
        &mut st,
        serde_json::from_value(json!({"title": "terms"})).unwrap(),
    )
    .unwrap();
    assert_eq!(p["changes"], 2);
}

#[test]
fn assets_and_schema() {
    let (_d, mut st) = state();
    start(&mut st);
    let a = rdy_asset(
        &mut st,
        serde_json::from_value(json!({"data_base64": "aGVsbG8="})).unwrap(),
    )
    .unwrap();
    assert_eq!(a["hash"].as_str().unwrap().len(), 64);
    let s = rdy_schema(
        &mut st,
        serde_json::from_value(json!({"block_type": "term"})).unwrap(),
    )
    .unwrap();
    assert_eq!(s["prefix"], "t");
    assert!(s["fields"]["name"]["required"].as_bool().unwrap());
    assert!(s["example"]["op"] == "create");
}

#[test]
fn budget_truncates_large_lists() {
    let big = json!({"items": (0..2000).map(|i| json!({"id": i, "summary": "x".repeat(40)})).collect::<Vec<_>>()});
    let out = budget(big, Some(500));
    assert_eq!(out["truncated"], true);
    assert!(out["omitted"].as_u64().unwrap() > 0);
    assert!(serde_json::to_string(&out).unwrap().len() / 4 <= 500);
    let text = budget(json!({"text": "é".repeat(10_000)}), Some(500));
    assert_eq!(text["truncated"], true);
    assert!(text["text"].as_str().unwrap().len() <= 2000);
    let small = budget(json!({"a": 1}), Some(500));
    assert!(small.get("truncated").is_none());
}

#[test]
fn budget_bounds_escaped_text() {
    let text = (0..5000)
        .map(|_| "line \"x\"")
        .collect::<Vec<_>>()
        .join("\n");
    let out = budget(json!({"text": text}), Some(500));
    assert_eq!(out["truncated"], true);
    assert!(serde_json::to_string(&out).unwrap().len() / 4 <= 500);
}

#[test]
fn failed_session_leaves_state_unchanged() {
    let (_d, mut st) = state();
    start(&mut st);
    for bad in [
        json!({"actor": "a", "role": "writer", "view": "canon"}),
        json!({"actor": "a", "role": "writer", "run": "canon"}),
        json!({"actor": "a", "role": "boss"}),
    ] {
        assert!(rdy_session(&mut st, serde_json::from_value(bad).unwrap()).is_err());
        assert_eq!(st.view, "run-7");
        assert_eq!(st.session.as_ref().unwrap().actor, "agent:designer");
    }
}

#[test]
fn read_rejects_unknown_format_and_yaml_separates() {
    let (_d, mut st) = state();
    start(&mut st);
    let res = seed(&mut st);
    let doc = res["created"]["$doc"].as_str().unwrap().to_string();
    let e = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": doc, "format": "xml"})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    let y = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": doc, "format": "yaml"})).unwrap(),
    )
    .unwrap();
    assert!(y["text"].as_str().unwrap().contains("---\n"));
    let md = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": doc, "format": "markdown"})).unwrap(),
    )
    .unwrap();
    assert!(md["text"].as_str().unwrap().contains("\n\n"));
}

#[test]
fn readers_cannot_write_or_upload() {
    let (_d, mut st) = state();
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:r", "role": "reader"})).unwrap(),
    )
    .unwrap();
    let e = rdy_write(&mut st, serde_json::from_value(json!({"ops": [{"op": "create", "type": "doc", "fields": {"title": "T", "doc_type": "glossary"}}]})).unwrap()).unwrap_err();
    assert_eq!(e.code, "E_PERMISSION");
    let e = rdy_asset(
        &mut st,
        serde_json::from_value(json!({"data_base64": "aGVsbG8="})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PERMISSION");
}

#[test]
fn justification_mapping_and_conflict() {
    use rdy_model::{Change, EditorialClass, Justification};
    let (_d, mut st) = state();
    start(&mut st);
    let res = seed(&mut st);
    let c = res["created"]["$c"].as_str().unwrap().to_string();
    let d = rdy_decide(
        &mut st,
        serde_json::from_value(json!({"parent": res["created"]["$s"], "statement": "S"})).unwrap(),
    )
    .unwrap();
    let did = d["created"]["$decision"].as_str().unwrap().to_string();
    let e = rdy_write(&mut st, serde_json::from_value(json!({"ops": [{"op": "update", "id": c, "body": "X."}], "implements": did, "editorial": {"class": "typo", "reason": "r"}})).unwrap()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    let last = |st: &McpState| {
        let v = st.rdy.store().view("run-7").unwrap();
        st.rdy
            .store()
            .get::<Change>(v.changes.last().unwrap())
            .unwrap()
    };
    rdy_write(&mut st, serde_json::from_value(json!({"ops": [{"op": "update", "id": c, "body": "Edited."}], "editorial": {"class": "clarify", "reason": "why"}})).unwrap()).unwrap();
    assert_eq!(
        last(&st).justification,
        Some(Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: "why".into()
        })
    );
    rdy_write(
        &mut st,
        serde_json::from_value(
            json!({"ops": [{"op": "update", "id": c, "body": "Edited again."}], "implements": did}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        last(&st).justification,
        Some(Justification::Implements(
            rdy_model::BlockId::parse(&did).unwrap()
        ))
    );
}

#[test]
fn compact_read_keeps_one_indented_line_per_block_with_multiline_diagrams() {
    let (_d, mut st) = state();
    start(&mut st);
    let res = rdy_write(&mut st, serde_json::from_value(json!({"ops": [
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Flows", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Login"}},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": "login flow"}, "body": "flowchart TD\n  A[Start] --> B{{Valid?}}\n  B --> C[Done]"},
        {"op": "create", "type": "claim", "parent": "$s", "body": "Sessions expire."}
    ]})).unwrap()).unwrap();
    let doc = res["created"]["$doc"].as_str().unwrap().to_string();
    let read = rdy_read(&mut st, serde_json::from_value(json!({"id": doc})).unwrap()).unwrap();
    let text = read["text"].as_str().unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "one line per block:\n{text}");
    for (line, depth) in lines.iter().zip([0, 1, 2, 2]) {
        let indent = "  ".repeat(depth);
        assert!(
            line.starts_with(&indent) && !line[indent.len()..].starts_with(' '),
            "depth {depth} indentation: {line:?}\n{text}"
        );
    }
}

fn approver() -> rdy_engine::Session {
    rdy_engine::Session {
        actor: "human:founder".into(),
        run: None,
        role: rdy_model::Role::Approver,
    }
}

/// Seeds the session view, lands it as the founder and tags it v0.1.0; returns the landing id.
fn land_and_tag(st: &mut McpState) -> String {
    seed(st);
    let p = rdy_propose(
        st,
        serde_json::from_value(json!({"title": "seed"})).unwrap(),
    )
    .unwrap();
    let landing = st.rdy.land(&approver(), p["id"].as_str().unwrap()).unwrap();
    st.rdy
        .tag(&approver(), "v0.1.0", None, Some("first".into()))
        .unwrap();
    landing
}

#[test]
fn run_ids_with_colons_become_view_names_without() {
    let (_d, mut st) = state();
    let r = rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:x", "run": "batch:7:a", "role": "writer"}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(r["view"], "batch-7-a");
    assert_eq!(st.view, "batch-7-a");
    let e = rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:x", "role": "writer", "view": "a:b"}))
            .unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_RESERVED_VIEW");
}

#[test]
fn rdy_tags_lists_versions() {
    let (_d, mut st) = state();
    start(&mut st);
    let landing = land_and_tag(&mut st);
    let mut v = rdy_tags(&mut st, TagsParams::default()).unwrap();
    v["versions"][0].as_object_mut().unwrap().remove("at_ms");
    assert_eq!(
        v,
        json!({
            "versions": [{"name": "v0.1.0", "landing": landing, "message": "first", "tagged_by": "human:founder"}],
            "current": "v0.1.0"
        })
    );
}

#[test]
fn overview_reports_the_version_position() {
    let (_d, mut st) = state();
    start(&mut st);
    let landing = land_and_tag(&mut st);
    let ov = rdy_overview(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert_eq!(
        ov["version_position"],
        json!({
            "version": {"name": "v0.1.0", "landing": landing},
            "landings_since": 0,
            "view": {"name": "run-7", "unlanded_changes": 0},
            "frozen": false,
            "label": "v0.1.0 · view run-7, 0 unlanded changes"
        })
    );
    let ov = rdy_overview(
        &mut st,
        serde_json::from_value(json!({"scope": "tag:v0.1.0"})).unwrap(),
    )
    .unwrap();
    assert_eq!(ov["version_position"]["label"], "v0.1.0 · frozen snapshot");
}

#[test]
fn read_as_of_a_version() {
    let (_d, mut st) = state();
    start(&mut st);
    land_and_tag(&mut st);
    let doc = st
        .rdy
        .materialized(&rdy_engine::Scope::Canon)
        .unwrap()
        .children(None)[0]
        .id
        .to_string();
    let section = st
        .rdy
        .materialized(&rdy_engine::Scope::Canon)
        .unwrap()
        .children(Some(&rdy_model::BlockId::parse(&doc).unwrap()))[0]
        .id
        .to_string();
    rdy_write(
        &mut st,
        serde_json::from_value(json!({"ops": [
            {"op": "create", "type": "claim", "parent": section, "body": "Added after the version."}
        ]}))
        .unwrap(),
    )
    .unwrap();
    let now = rdy_read(&mut st, serde_json::from_value(json!({"id": doc})).unwrap()).unwrap();
    assert!(
        now["text"]
            .as_str()
            .unwrap()
            .contains("Added after the version.")
    );
    let then = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": doc, "scope": "tag:v0.1.0"})).unwrap(),
    )
    .unwrap();
    assert!(
        !then["text"]
            .as_str()
            .unwrap()
            .contains("Added after the version.")
    );
}

fn folder_tree(st: &mut McpState) -> Value {
    rdy_write(st, serde_json::from_value(json!({"ops": [
        {"op": "create", "type": "doc", "id": "$g", "fields": {"title": "Glossary", "doc_type": "glossary"}},
        {"op": "create", "type": "folder", "id": "$w", "fields": {"name": "wallets"}},
        {"op": "create", "type": "doc", "id": "$t", "parent": "$w", "fields": {"title": "Transactions", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$t", "fields": {"title": "Summary"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Moves money."}
    ]})).unwrap()).unwrap()
}

#[test]
fn overview_returns_the_folder_tree() {
    let (_d, mut st) = state();
    start(&mut st);
    let res = folder_tree(&mut st);
    let c = |k: &str| res["created"][k].as_str().unwrap().to_string();
    let ov = rdy_overview(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert!(ov.get("docs").is_none());
    assert_eq!(
        ov["tree"],
        json!([
            {"path": "glossary", "name": "glossary", "id": c("$g"), "type": "doc", "title": "Glossary", "doc_type": "glossary"},
            {"path": "wallets", "name": "wallets", "id": c("$w"), "type": "folder", "children": [
                {"path": "wallets/transactions", "name": "transactions", "id": c("$t"), "type": "doc", "title": "Transactions", "doc_type": "overview"}
            ]}
        ])
    );
}

#[test]
fn overview_collapses_the_deepest_levels_first() {
    let (_d, mut st) = state();
    start(&mut st);
    let mut ops =
        vec![json!({"op": "create", "type": "folder", "id": "$a", "fields": {"name": "a"}})];
    for i in 0..60 {
        ops.push(json!({"op": "create", "type": "doc", "parent": "$a", "fields": {"title": format!("A long document title number {i}"), "doc_type": "glossary"}}));
    }
    rdy_write(
        &mut st,
        serde_json::from_value(json!({"ops": ops})).unwrap(),
    )
    .unwrap();
    let ov = rdy_overview(
        &mut st,
        serde_json::from_value(json!({"max_tokens": 400})).unwrap(),
    )
    .unwrap();
    assert_eq!(ov["tree"][0]["path"], "a");
    assert!(ov["tree"][0].get("children").is_none(), "{}", ov["tree"]);
    assert_eq!(ov["tree"][0]["collapsed"], 60);
}

#[test]
fn tools_take_paths() {
    let (_d, mut st) = state();
    start(&mut st);
    let res = folder_tree(&mut st);
    let read = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": "wallets/transactions"})).unwrap(),
    )
    .unwrap();
    assert!(read["text"].as_str().unwrap().contains("Moves money."));
    let ctx = rdy_context(
        &mut st,
        serde_json::from_value(json!({"id": "wallets/transactions"})).unwrap(),
    )
    .unwrap();
    assert_eq!(ctx["id"], res["created"]["$t"]);
    rdy_impact(
        &mut st,
        serde_json::from_value(json!({"id": "glossary"})).unwrap(),
    )
    .unwrap();
    // folders are written with the normal ops; parent takes a path
    rdy_write(&mut st, serde_json::from_value(json!({"ops": [
        {"op": "create", "type": "folder", "id": "$x", "parent": "wallets", "fields": {"name": "cards"}},
        {"op": "move", "id": "glossary", "parent": "wallets/cards"}
    ]})).unwrap()).unwrap_err(); // $x is not visible to paths in the same write: refused
    rdy_write(
        &mut st,
        serde_json::from_value(json!({"ops": [
            {"op": "create", "type": "folder", "parent": "wallets", "fields": {"name": "cards"}}
        ]}))
        .unwrap(),
    )
    .unwrap();
    rdy_write(
        &mut st,
        serde_json::from_value(
            json!({"ops": [{"op": "move", "id": "glossary", "parent": "wallets/cards"}]}),
        )
        .unwrap(),
    )
    .unwrap();
    let ctx = rdy_context(
        &mut st,
        serde_json::from_value(json!({"id": "wallets/cards/glossary"})).unwrap(),
    )
    .unwrap();
    assert_eq!(ctx["id"], res["created"]["$g"]);
}

#[test]
fn name_errors_carry_a_corrected_payload_and_schema_explains_names() {
    let (_d, mut st) = state();
    start(&mut st);
    folder_tree(&mut st);
    let e = rdy_write(
        &mut st,
        serde_json::from_value(json!({"ops": [
            {"op": "create", "type": "folder", "fields": {"name": "Wallets"}}
        ]}))
        .unwrap(),
    )
    .unwrap_err();
    let v = error_json(&e);
    assert_eq!(v["code"], "E_PATH_CLASH");
    assert_eq!(
        v["corrected"],
        json!({"op": 0, "fields": {"name": "Wallets-2"}})
    );
    let s = rdy_schema(
        &mut st,
        serde_json::from_value(json!({"block_type": "folder"})).unwrap(),
    )
    .unwrap();
    assert!(s["name_rules"].as_str().unwrap().contains("ignoring case"));
    assert_eq!(s["children"], json!(["folder", "doc"]));
    let all = rdy_schema(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert!(
        all["types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["type"] == "folder")
    );
}
