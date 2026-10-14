use rdy_engine::Rdy;
use rdy_engine::engine::{Local, write_typed};
use rdy_mcp::tools::*;
use serde_json::{Value, json};

fn state() -> (tempfile::TempDir, McpState) {
    let dir = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, McpState::new(std::sync::Arc::new(Local::new(rdy))))
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
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "page"}},
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
        text.contains(&format!("{c} claim: Uses the {t}.")),
        "read1: references stay as bare ids:\n{text}"
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
    let e = rdy_write(&mut st, serde_json::from_value(json!({"ops": [{"op": "create", "type": "doc", "fields": {"title": "T", "doc_type": "page"}}]})).unwrap()).unwrap_err();
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
        let r = st.engine.read();
        let v = r.reader().view("run-7").unwrap();
        r.reader().get::<Change>(v.changes.last().unwrap()).unwrap()
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
        Some(Justification::Implements(vec![
            rdy_model::BlockId::parse(&did).unwrap()
        ]))
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
        kind: rdy_engine::SessionKind::Human,
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
    let pid = p["id"].as_str().unwrap().to_string();
    let landing = write_typed(&*st.engine, move |r| r.land(&approver(), &pid)).unwrap();
    write_typed(&*st.engine, |r| {
        r.tag(&approver(), "v0.1.0", None, Some("first".into()))
    })
    .unwrap();
    landing.as_str().unwrap().to_string()
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
    let row = v["versions"][0].as_object_mut().unwrap();
    row.remove("at_ms");
    // `tag` is the tag object's full hash: a 64-hex id, not the landing it names.
    let tag = row.remove("tag").unwrap();
    let tag = tag.as_str().unwrap();
    assert_eq!(tag.len(), 64);
    assert!(tag.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_ne!(tag, landing);
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
        .engine
        .read()
        .materialized(&rdy_engine::Scope::Canon)
        .unwrap()
        .children(None)[0]
        .id
        .to_string();
    let section = st
        .engine
        .read()
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
        {"op": "create", "type": "doc", "id": "$g", "fields": {"title": "Glossary", "doc_type": "page"}},
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
            {"path": "glossary", "name": "glossary", "id": c("$g"), "type": "doc", "title": "Glossary", "doc_type": "page"},
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
        ops.push(json!({"op": "create", "type": "doc", "parent": "$a", "fields": {"title": format!("A long document title number {i}"), "doc_type": "page"}}));
    }
    rdy_write(
        &mut st,
        serde_json::from_value(json!({"ops": ops})).unwrap(),
    )
    .unwrap();
    let ov = rdy_overview(
        &mut st,
        serde_json::from_value(json!({"max_tokens": 1000})).unwrap(),
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

/// `rdy_overview` counts each view's changes from its included set, and (df9) lists only a view
/// that holds at least one: after the landing `run-7` is open and empty, so it is not listed.
#[test]
fn chain1_overview_counts_each_views_changes() {
    let (_d, mut st) = state();
    start(&mut st);
    seed(&mut st);
    rdy_write(
        &mut st,
        serde_json::from_value(json!({"ops": [
            {"op": "create", "type": "doc", "fields": {"title": "Second", "doc_type": "page"}}
        ]}))
        .unwrap(),
    )
    .unwrap();
    let views = |st: &mut McpState| {
        let ov = rdy_overview(st, serde_json::from_value(json!({})).unwrap()).unwrap();
        ov["views"].clone()
    };
    assert_eq!(views(&mut st), json!([{"name": "run-7", "changes": 2}]));
    let p = rdy_propose(
        &mut st,
        serde_json::from_value(json!({"title": "two"})).unwrap(),
    )
    .unwrap();
    let pid = p["id"].as_str().unwrap().to_string();
    write_typed(&*st.engine, move |r| r.land(&approver(), &pid)).unwrap();
    assert_eq!(views(&mut st), json!([]));
}

/// Final review fix round 1 (minor 9): the schema says in words that the daemon's socket carries
/// the same request-line limit as stdio.
#[test]
fn schema1_the_transport_note_says_the_socket_has_the_stdio_line_limit() {
    let (_d, mut st) = state();
    let all = rdy_schema(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    let limits = &all["limits"];
    assert_eq!(limits["socket_line_bytes"], limits["request_bytes"]);
    let note = limits["transport"].as_str().expect("a transport note");
    assert!(note.contains("socket"), "{note}");
    assert!(note.contains("stdio"), "{note}");
    assert!(note.contains("16 MiB"), "{note}");
}

#[test]
fn rpag1_a_block_larger_than_the_budget_is_shown_alone_and_cut() {
    let (_d, mut st) = state();
    start(&mut st);
    let big = "word ".repeat(2_000);
    let res = rdy_write(&mut st, serde_json::from_value(json!({"ops": [
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Big", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Body"}},
        {"op": "create", "type": "code", "id": "$k", "parent": "$s", "fields": {"lang": "text"}, "body": big},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "After the big block."}
    ]})).unwrap()).unwrap();
    let doc = res["created"]["$doc"].as_str().unwrap().to_string();
    let k = res["created"]["$k"].as_str().unwrap().to_string();
    let read = |st: &mut McpState, args: Value| rdy_read(st, serde_json::from_value(args).unwrap());
    let mut v = read(&mut st, json!({"id": doc, "max_tokens": 500})).unwrap();
    let mut texts = vec![];
    while let Some(next) = v["next"].as_str().map(String::from) {
        assert!(serde_json::to_string(&v).unwrap().len() / 4 <= 500, "{v}");
        texts.push(v["text"].as_str().unwrap().to_string());
        v = read(&mut st, json!({"page": next, "max_tokens": 500})).unwrap();
    }
    texts.push(v["text"].as_str().unwrap().to_string());
    let cut = texts.iter().find(|t| t.contains(&k)).unwrap();
    assert!(
        cut.trim_start().starts_with(&k) && cut.len() < big.len(),
        "{cut}"
    );
    assert!(
        texts.last().unwrap().contains("After the big block."),
        "{texts:?}"
    );
}

#[test]
fn rpag1_a_cut_block_names_itself_and_the_bytes_cut() {
    let (_d, mut st) = state();
    start(&mut st);
    let big: String = (0..1_200)
        .map(|i| format!("key_{i} = \"value {i}\"\n"))
        .collect();
    let res = rdy_write(&mut st, serde_json::from_value(json!({"ops": [
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Big", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Body"}},
        {"op": "create", "type": "code", "id": "$k", "parent": "$s", "fields": {"lang": "toml"}, "body": big},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "After the big block."},
        {"op": "create", "type": "doc", "id": "$tail", "fields": {"title": "Tail", "doc_type": "page"}},
        {"op": "create", "type": "code", "id": "$k2", "parent": "$tail", "fields": {"lang": "toml"}, "body": big}
    ]})).unwrap()).unwrap();
    let id = |k: &str| res["created"][k].as_str().unwrap().to_string();
    let read = |st: &mut McpState, args: Value| rdy_read(st, serde_json::from_value(args).unwrap());
    // the bytes of block `k`'s entry in the uncut read of `doc`, which ends where `after` begins
    let entry_len = |st: &mut McpState, doc: &str, k: &str, after: Option<&str>| {
        let full = read(st, json!({"id": id(doc), "max_tokens": 100_000})).unwrap();
        assert!(
            full.get("cut").is_none() && full.get("truncated").is_none(),
            "{full}"
        );
        let text = full["text"].as_str().unwrap();
        let line_start = |head: String| text[..text.find(&head).unwrap()].rfind('\n').unwrap();
        let start = line_start(format!("{} code", id(k))) + 1;
        let end = after.map_or(text.len(), |a| line_start(format!("{} claim", id(a))));
        end - start
    };
    let shown = |v: &Value| v["text"].as_str().unwrap().len();
    // a cut block before the last page: next and omitted (entries) as usual, and cut names it
    let whole = entry_len(&mut st, "$doc", "$k", Some("$c"));
    let mut v = read(&mut st, json!({"id": id("$doc")})).unwrap();
    while !v["text"].as_str().unwrap().contains(&id("$k")) {
        v = read(&mut st, json!({"page": v["next"]})).unwrap();
    }
    assert!(serde_json::to_string(&v).unwrap().len() / 4 <= 4000, "{v}");
    assert_eq!(v["cut"]["id"], id("$k"), "{v}");
    assert_eq!(
        v["cut"]["omitted_bytes"].as_u64().unwrap() as usize,
        whole - shown(&v),
        "{v}"
    );
    assert_eq!(
        (v["truncated"].as_bool(), v["omitted"].as_u64()),
        (Some(true), Some(1)),
        "{v}"
    );
    let last = read(&mut st, json!({"page": v["next"]})).unwrap();
    assert!(
        last.get("cut").is_none() && last.get("truncated").is_none(),
        "{last}"
    );
    // a cut block on the last page: no next, and omitted never counts bytes
    let whole = entry_len(&mut st, "$tail", "$k2", None);
    let mut v = read(&mut st, json!({"id": id("$tail")})).unwrap();
    while let Some(next) = v["next"].as_str().map(String::from) {
        v = read(&mut st, json!({"page": next})).unwrap();
    }
    assert_eq!(v["cut"]["id"], id("$k2"), "{v}");
    assert_eq!(
        v["cut"]["omitted_bytes"].as_u64().unwrap() as usize,
        whole - shown(&v),
        "{v}"
    );
    assert_eq!(v["truncated"], true, "{v}");
    assert!(v.get("omitted").is_none(), "{v}");
}

/// ib3 (cc6, FW-H7): coverage is for the current state only, so `rdy_overview` at a past
/// landing leaves coverage out, as briefings, search hits and fact sheets do (it no longer
/// refuses); at canon's head it serves canon's coverage.
#[test]
fn ib3_overview_at_a_past_landing_leaves_coverage_out() {
    let (_d, mut st) = state();
    start(&mut st);
    let first = land_and_tag(&mut st);
    rdy_write(&mut st, serde_json::from_value(json!({"ops": [
        {"op": "create", "type": "doc", "id": "$d2", "fields": {"title": "Later", "doc_type": "page"}}
    ]})).unwrap())
    .unwrap();
    let p = rdy_propose(
        &mut st,
        serde_json::from_value(json!({"title": "later"})).unwrap(),
    )
    .unwrap();
    let pid = p["id"].as_str().unwrap().to_string();
    let head = write_typed(&*st.engine, move |r| r.land(&approver(), &pid)).unwrap();
    let head = head.as_str().unwrap().to_string();
    for scope in ["tag:v0.1.0".to_string(), format!("landing:{first}")] {
        let v = rdy_overview(
            &mut st,
            serde_json::from_value(json!({"scope": scope})).unwrap(),
        )
        .unwrap();
        assert!(v.get("coverage").is_none(), "{scope}: {v}");
        assert!(v.get("coverage_error").is_none(), "{scope}: {v}");
        assert!(v.get("tree").is_some(), "{scope}: {v}");
        assert!(v.get("version_position").is_some(), "{scope}: {v}");
    }
    let at_head = rdy_overview(
        &mut st,
        serde_json::from_value(json!({"scope": format!("landing:{head}")})).unwrap(),
    )
    .unwrap();
    let canon = rdy_overview(
        &mut st,
        serde_json::from_value(json!({"scope": "canon"})).unwrap(),
    )
    .unwrap();
    assert_eq!(at_head["coverage"], canon["coverage"]);
    assert!(canon["coverage"]["score"].is_number(), "{canon}");
    assert_eq!(at_head["coverage"]["score"], canon["coverage"]["score"]);
}

/// A tension detector that always fails: every tension read of the state errs.
struct Failing;
static FAILING_DETS: [&dyn rdy_engine::tensions::Detector; 1] = [&Failing];
impl rdy_engine::tensions::Detector for Failing {
    fn kind(&self) -> rdy_engine::tensions::TensionKind {
        rdy_engine::tensions::TensionKind::EditCollision
    }
    fn detect(
        &self,
        _cx: &rdy_engine::tensions::DetectCx<'_>,
        _seeds: Option<&std::collections::BTreeSet<rdy_model::BlockId>>,
        _out: &mut Vec<rdy_engine::tensions::Found>,
    ) -> Result<(), rdy_engine::RdyError> {
        Err(rdy_engine::RdyError::new(
            rdy_engine::E_INTERNAL,
            "boom",
            "a test detector",
        ))
    }
}

/// cc15: a coverage read error never fails `rdy_overview`: the coverage is left out and
/// `coverage_error` says why. A past landing is still refused.
#[test]
fn cc15_a_coverage_read_error_never_fails_the_overview() {
    let dir = tempfile::tempdir().unwrap();
    let mut rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
    rdy.set_detectors_for_tests(&FAILING_DETS);
    let mut st = McpState::new(std::sync::Arc::new(Local::new(rdy)));
    start(&mut st);
    let ov = rdy_overview(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert!(ov.get("coverage").is_none(), "{ov:#}");
    assert!(
        ov["coverage_error"]
            .as_str()
            .is_some_and(|m| m.starts_with("E_INTERNAL: ") && m.contains("boom")),
        "{ov:#}"
    );
    assert_eq!(
        ov["pack"], "rdy-standard",
        "the rest of the overview stands"
    );
}

/// cc15 (perf fix): `rdy_overview` reads the memoised coverage summary and never builds the
/// full report; its numbers are `rdy_coverage`'s.
#[test]
fn cc15_overview_builds_no_coverage_report_and_agrees_with_it() {
    let (_d, mut st) = state();
    start(&mut st);
    seed(&mut st);
    let before = rdy_engine::coverage::report_builds();
    let ov = rdy_overview(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert_eq!(rdy_engine::coverage::report_builds(), before, "{ov:#}");
    let report = rdy_coverage(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    for k in [
        "score",
        "dimensions",
        "issues_total",
        "kinds",
        "similar_kinds",
    ] {
        assert_eq!(ov["coverage"][k], report[k], "{k}: {ov:#} vs {report:#}");
    }
}
