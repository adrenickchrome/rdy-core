//! adv1: an advisory warning is reported once per doc per MCP session; dtype1: a doc warns only
//! for its own type's skeleton, and rdy-standard's `page` type has none.
use rdy_engine::Rdy;
use rdy_engine::engine::Local;
use rdy_mcp::tools::*;
use serde_json::{Value, json};

fn session(st: &mut McpState) {
    rdy_session(
        st,
        serde_json::from_value(
            json!({"actor": "agent:designer", "run": "run-7", "role": "writer"}),
        )
        .unwrap(),
    )
    .unwrap();
}

fn state() -> (tempfile::TempDir, McpState) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut st = McpState::new(std::sync::Arc::new(Local::new(r)));
    session(&mut st);
    (dir, st)
}

fn write(st: &mut McpState, ops: Value) -> Value {
    rdy_write(st, serde_json::from_value(json!({"ops": ops})).unwrap()).unwrap()
}

fn skeleton(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "W_SKELETON_MISSING")
        .map(|w| w["message"].as_str().unwrap().to_string())
        .collect()
}

fn doc(st: &mut McpState, title: &str, doc_type: &str) -> (Value, String) {
    let v = write(
        st,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": title, "doc_type": doc_type}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Notes"}}
        ]),
    );
    let s = v["created"]["$s"].as_str().unwrap().to_string();
    (v, s)
}

fn claim(st: &mut McpState, parent: &str, body: &str) -> Value {
    write(
        st,
        json!([{"op": "create", "type": "claim", "parent": parent, "body": body}]),
    )
}

#[test]
fn adv1_a_skeleton_warning_is_reported_once_per_doc_per_session() {
    let (_d, mut st) = state();
    let (first, s) = doc(&mut st, "Plan", "overview");
    assert_eq!(
        skeleton(&first),
        vec![
            "required section \"Summary\" is missing",
            "required section \"Goals\" is missing"
        ],
        "{first}"
    );
    assert!(first.get("warnings_repeated").is_none(), "{first}");
    let again = claim(&mut st, &s, "One.");
    assert!(skeleton(&again).is_empty(), "{again}");
    assert_eq!(again["warnings_repeated"], 2, "{again}");
    assert!(again["warnings"].is_array(), "the key stays: {again}");
    let (other, _) = doc(&mut st, "Plan two", "overview");
    assert_eq!(
        skeleton(&other).len(),
        2,
        "another doc warns on its own: {other}"
    );
    session(&mut st);
    let fresh = claim(&mut st, &s, "Two.");
    assert_eq!(
        skeleton(&fresh).len(),
        2,
        "a new session hears it again: {fresh}"
    );
}

#[test]
fn adv1_only_advisories_are_deduplicated() {
    assert_eq!(ADVISORY_ONCE, ["W_SKELETON_MISSING"]);
}

#[test]
fn dtype1_a_doc_warns_only_for_its_own_types_skeleton() {
    let (_d, mut st) = state();
    let (page, _) = doc(&mut st, "Free form", "page");
    assert!(skeleton(&page).is_empty(), "page has no skeleton: {page}");
    let (glossary, _) = doc(&mut st, "Words", "glossary");
    assert_eq!(
        skeleton(&glossary),
        vec!["required section \"Terms\" is missing"],
        "only glossary's own section, never overview's: {glossary}"
    );
}

#[test]
fn adv1_rdy_schema_names_the_skeleton_warning_and_its_repeat_count() {
    let (_d, mut st) = state();
    let s = rdy_schema(&mut st, SchemaParams::default()).unwrap();
    let row = &s["errors"]["W_SKELETON_MISSING"];
    assert!(
        row["when"].as_str().is_some_and(
            |w| w.contains("once per doc per session") && w.contains("warnings_repeated")
        ),
        "{row}"
    );
    assert!(row["fix"].is_string(), "{row}");
}
