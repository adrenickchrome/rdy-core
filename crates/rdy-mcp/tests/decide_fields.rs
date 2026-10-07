//! dfld1: rdy_decide (and rdy_supersede's decision) take pack-defined fields, validated against
//! the pack as rdy_write validates fields, so one call records a decision.
use rdy_engine::engine::Local;
use rdy_engine::{Rdy, RdyError, Scope};
use rdy_mcp::tools::*;
use rdy_model::BlockId;
use serde_json::{Value, json};

/// rdy-design's decision extension (its pack, `.rdy/packs/rdy-design.yaml`), under another name.
const DESIGN_PACK: &str = "name: ae-design\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n      disposition: { kind: str }\n      trigger: { kind: str }\n";

fn state(design: bool) -> (tempfile::TempDir, McpState) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    if design {
        drop(r);
        std::fs::write(dir.path().join(".rdy/packs/ae-design.yaml"), DESIGN_PACK).unwrap();
        std::fs::write(dir.path().join(".rdy/config.yaml"), "pack: ae-design\n").unwrap();
        r = Rdy::open(dir.path()).unwrap();
    }
    let mut st = McpState::new(std::sync::Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(
            json!({"actor": "agent:designer", "run": "run-7", "role": "writer"}),
        )
        .unwrap(),
    )
    .unwrap();
    (dir, st)
}

fn section(st: &mut McpState) -> String {
    let res = rdy_write(st, serde_json::from_value(json!({"ops": [
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Register", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Rows"}}
    ]})).unwrap()).unwrap();
    res["created"]["$s"].as_str().unwrap().to_string()
}

fn decide(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    rdy_decide(st, serde_json::from_value(args).unwrap())
}

fn field(st: &McpState, id: &str, name: &str) -> Option<String> {
    let b = st
        .engine
        .read()
        .block(&Scope::View(st.view.clone()), &BlockId::parse(id).unwrap())
        .unwrap();
    b.field_str(name).map(String::from)
}

fn created(v: &Value) -> String {
    v["created"]["$decision"].as_str().unwrap().to_string()
}

#[test]
fn dfld1_rdy_decide_records_pack_fields_in_one_call() {
    let (_d, mut st) = state(true);
    let s = section(&mut st);
    let d = created(
        &decide(
            &mut st,
            json!({"parent": s, "statement": "Pins record content",
                   "fields": {"key": "FW-E3", "disposition": "built in M2c3", "trigger": "a pin drifts"}}),
        )
        .unwrap(),
    );
    assert_eq!(field(&st, &d, "key").as_deref(), Some("FW-E3"));
    assert_eq!(
        field(&st, &d, "disposition").as_deref(),
        Some("built in M2c3")
    );
    assert_eq!(field(&st, &d, "trigger").as_deref(), Some("a pin drifts"));
    assert_eq!(
        field(&st, &d, "statement").as_deref(),
        Some("Pins record content")
    );
    assert_eq!(field(&st, &d, "status").as_deref(), Some("proposed"));
}

#[test]
fn dfld1_pack_fields_are_validated_like_rdy_write() {
    let issue = |e: &RdyError| {
        e.issues
            .iter()
            .map(|i| i.code.to_string())
            .collect::<Vec<_>>()
    };
    // rdy-standard's decision has no `trigger`
    let (_d, mut st) = state(false);
    let s = section(&mut st);
    let e = decide(
        &mut st,
        json!({"parent": s, "statement": "x", "fields": {"trigger": "t"}}),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e}");
    assert!(
        issue(&e).contains(&"E_FIELD_UNKNOWN".to_string()),
        "{:?}",
        issue(&e)
    );
    // the design pack's `disposition` is a str; a key must be a key
    let (_d2, mut st) = state(true);
    let s = section(&mut st);
    let e = decide(
        &mut st,
        json!({"parent": s, "statement": "x", "fields": {"disposition": 5}}),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e}");
    assert!(
        issue(&e).contains(&"E_FIELD_KIND".to_string()),
        "{:?}",
        issue(&e)
    );
    let e = decide(
        &mut st,
        json!({"parent": s, "statement": "x", "fields": {"key": "not a key!"}}),
    )
    .unwrap_err();
    assert!(
        e.code == "E_BAD_KEY" || issue(&e).contains(&"E_BAD_KEY".to_string()),
        "{e} {:?}",
        issue(&e)
    );
    // a taken key, as rdy_write refuses it; the refused decisions wrote nothing
    decide(
        &mut st,
        json!({"parent": s, "statement": "first", "fields": {"key": "FW-E3"}}),
    )
    .unwrap();
    let e = decide(
        &mut st,
        json!({"parent": s, "statement": "second", "fields": {"key": "fw-e3"}}),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_KEY_CLASH", "{e}");
    let read = rdy_read(&mut st, serde_json::from_value(json!({"id": s})).unwrap()).unwrap();
    assert_eq!(
        read["text"]
            .as_str()
            .unwrap()
            .matches(" decision: ")
            .count(),
        1,
        "only the accepted call wrote a decision: {read}"
    );
}

#[test]
fn dfld1_fields_cannot_set_what_rdy_decide_owns() {
    let (_d, mut st) = state(true);
    let s = section(&mut st);
    for name in [
        "statement",
        "status",
        "options",
        "kind",
        "effective_from",
        "decided_at",
        "recorded_at",
    ] {
        let mut fields = serde_json::Map::new();
        fields.insert(name.to_string(), json!("x"));
        let e = decide(
            &mut st,
            json!({"parent": s, "statement": "x", "fields": fields}),
        )
        .unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{name}: {e}");
        assert_eq!(
            e.message,
            format!("fields.{name} is set by rdy_decide, not through fields")
        );
        assert_eq!(
            e.fix,
            "pass statement, options, authority_basis, attested_by, reversibility, kind, review_by and effective_from as their own parameters; rdy sets status, decided_at and recorded_at"
        );
    }
}

#[test]
fn dfld1_rdy_supersede_takes_fields_on_the_new_decision() {
    let (_d, mut st) = state(true);
    let s = section(&mut st);
    let old = created(
        &decide(
            &mut st,
            json!({"parent": s, "statement": "Pins record versions",
            "fields": {"key": "FW-E3", "disposition": "M2c"}}),
        )
        .unwrap(),
    );
    let res = rdy_supersede(
        &mut st,
        serde_json::from_value(json!({"old": old, "decision": {"parent": s,
            "statement": "Pins record content", "fields": {"key": "FW-E3b", "disposition": "M2c3"}}}))
        .unwrap(),
    )
    .unwrap();
    let new = created(&res);
    assert_eq!(field(&st, &new, "disposition").as_deref(), Some("M2c3"));
    assert_eq!(field(&st, &new, "key").as_deref(), Some("FW-E3b"));
    assert_eq!(
        field(&st, &old, "disposition").as_deref(),
        Some("M2c"),
        "fields are not inherited, and the old decision keeps its own"
    );
}
