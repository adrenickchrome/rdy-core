//! ab1 (D-WJ0SJ): a new decision records its build status once, at create, and the status is a
//! tracking field afterwards: it changes on an accepted decision with an editorial reason, leaves
//! the ruling frozen and the pins of its citers alone, and keeps its history.
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::tools::{self, McpState};
use rdy_model::{BlockId, EditorialClass, FieldValue, Justification};
use rdy_qa::fixtures::*;
use serde_json::{Value, from_value, json};
use std::path::Path;
use std::sync::Arc;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// An MCP session as a writer on view `ra` over the project in `dir`.
fn mcp(dir: &Path) -> McpState {
    let mut st = McpState::new(Arc::new(Local::open(dir).unwrap()));
    tools::rdy_session(
        &mut st,
        from_value(json!({"actor": "agent:a", "run": "ra", "role": "writer"})).unwrap(),
    )
    .unwrap();
    st
}

fn built_in(r: &Rdy, scope: &Scope, id_text: &str) -> Option<String> {
    r.read()
        .block(scope, &id(id_text))
        .unwrap()
        .field_str("built")
        .map(str::to_string)
}

fn decide(st: &mut McpState, params: Value) -> Result<Value, RdyError> {
    tools::rdy_decide(st, from_value(params).unwrap())
}

fn created(v: &Value, key: &str) -> String {
    v["created"][key].as_str().unwrap().to_string()
}

fn edit(
    r: &mut Rdy,
    view: &str,
    dec: &str,
    fields: Value,
    reason: Option<&str>,
) -> Result<WriteResult, RdyError> {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: from_value(json!([{"op": "update", "id": dec, "fields": fields}])).unwrap(),
            justification: reason.map(|w| Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: w.into(),
            }),
            message: String::new(),
        },
    )
}

#[test]
fn ab1_create_defaults_once_and_tracking_keeps_history() {
    let (dir, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    drop(r);

    // MCP rdy_decide and rdy_write create decisions: planned by default, none for a decision
    // that needs no content, an explicit value always wins
    let mut st = mcp(dir.path());
    let plain = decide(&mut st, json!({"statement": "Ship weekly"})).unwrap();
    let process = decide(
        &mut st,
        json!({"statement": "Meet on Mondays", "fields": {"needs_content": false}}),
    )
    .unwrap();
    let explicit = decide(
        &mut st,
        json!({"statement": "Ship the CLI", "fields": {"built": "v0.35.0"}}),
    )
    .unwrap();
    let explicit_wins = decide(
        &mut st,
        json!({"statement": "Plan the hub", "fields": {"needs_content": false, "built": "planned"}}),
    )
    .unwrap();
    let needs = decide(
        &mut st,
        json!({"statement": "Ship docs", "fields": {"needs_content": true}}),
    )
    .unwrap();
    let written = tools::rdy_write(
        &mut st,
        from_value(json!({"ops": [
            {"op": "create", "type": "decision", "id": "$w", "fields": {"statement": "Written", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$n", "fields": {"statement": "Written, no content", "status": "proposed", "needs_content": false}},
            {"op": "create", "type": "claim", "id": "$c", "parent": section, "body": "A claim."}
        ]}))
        .unwrap(),
    )
    .unwrap();
    // a bad value is refused with the schema's words, and nothing is written
    let e = decide(
        &mut st,
        json!({"statement": "Bad", "fields": {"built": "soon"}}),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VALIDATION);
    assert_eq!(
        (
            e.issues[0].code,
            e.issues[0].message.as_str(),
            e.issues[0].fix.as_str()
        ),
        (
            "E_BAD_INPUT",
            "Field \"built\" must be planned, none or a release version.",
            "Write built as planned, none, or a version such as 1.2.0."
        )
    );
    drop(st);

    let mut r = Rdy::open(dir.path()).unwrap();
    let view = Scope::View("ra".into());
    let want = [
        (created(&plain, "$decision"), "planned"),
        (created(&process, "$decision"), "none"),
        (created(&explicit, "$decision"), "v0.35.0"),
        (created(&explicit_wins, "$decision"), "planned"),
        (created(&needs, "$decision"), "planned"),
        (created(&written, "$w"), "planned"),
        (created(&written, "$n"), "none"),
    ];
    for (dec, built) in &want {
        assert_eq!(built_in(&r, &view, dec).as_deref(), Some(*built), "{dec}");
    }
    // a claim never gets built
    let claim = r
        .read()
        .block(&view, &id(&created(&written, "$c")))
        .unwrap();
    assert!(!claim.fields.contains_key("built"));
    // a later needs_content edit on a proposed decision leaves built where it was
    let process_id = created(&process, "$decision");
    edit(
        &mut r,
        "ra",
        &process_id,
        json!({"needs_content": true}),
        None,
    )
    .unwrap();
    assert_eq!(built_in(&r, &view, &process_id).as_deref(), Some("none"));
    let plain_id = created(&plain, "$decision");
    edit(
        &mut r,
        "ra",
        &plain_id,
        json!({"needs_content": false}),
        None,
    )
    .unwrap();
    assert_eq!(built_in(&r, &view, &plain_id).as_deref(), Some("planned"));
    // a value edit is checked like a create
    let e = edit(&mut r, "ra", &plain_id, json!({"built": "1.2"}), None).unwrap_err();
    assert_eq!(e.code, E_VALIDATION);
    land(&mut r, "ra").unwrap();

    // supersede and revoke create decisions that start planned, whatever the old one holds
    let old = created(&explicit, "$decision");
    assert_eq!(
        built_in(&r, &Scope::Canon, &old).as_deref(),
        Some("v0.35.0")
    );
    drop(r);
    let mut st = mcp(dir.path());
    let sup = tools::rdy_supersede(
        &mut st,
        from_value(json!({"old": old, "decision": {"statement": "Ship the CLI twice"}})).unwrap(),
    )
    .unwrap();
    let rev = tools::rdy_revoke(
        &mut st,
        from_value(json!({"target": created(&needs, "$decision"), "reason": "not needed"}))
            .unwrap(),
    )
    .unwrap();
    drop(st);
    let mut r = Rdy::open(dir.path()).unwrap();
    for key in [created(&sup, "$decision"), created(&rev, "$decision")] {
        assert_eq!(
            built_in(&r, &view, &key).as_deref(),
            Some("planned"),
            "{key}"
        );
    }
    land(&mut r, "ra").unwrap();

    // after acceptance, built is a tracking field: an editorial reason moves it, the ruling stays
    let dec = created(&plain, "$decision");
    let b = r.read().block(&Scope::Canon, &id(&dec)).unwrap();
    assert_eq!(b.field_str("status"), Some("accepted"));
    let e = edit(&mut r, "reg", &dec, json!({"built": "0.38.0"}), None).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.contains("editorial reason"), "{}", e.message);
    for fields in [
        json!({"statement": "Ship daily", "built": "0.38.0"}),
        json!({"needs_content": true}),
    ] {
        assert_eq!(
            edit(&mut r, "reg", &dec, fields.clone(), Some("tidy"))
                .unwrap_err()
                .code,
            E_FROZEN,
            "{fields}"
        );
    }
    // a claim that cites the decision keeps its pin: no suspect after the edit
    let cited = write(&mut r, "reg", json!([{"op": "create", "type": "claim", "id": "$k", "parent": section, "body": "We ship weekly.", "links": {"decided_by": [{"target": dec}]}}])).unwrap();
    land(&mut r, "reg").unwrap();
    let claim = created_id(&cited, "$k");
    let suspect = |r: &Rdy| {
        r.read()
            .suspects(&Scope::Canon)
            .unwrap()
            .iter()
            .any(|s| s.block == claim)
    };
    assert!(!suspect(&r));
    edit(
        &mut r,
        "reg2",
        &dec,
        json!({"built": "0.38.0"}),
        Some("shipped in 0.38.0"),
    )
    .unwrap();
    land(&mut r, "reg2").unwrap();
    assert!(!suspect(&r), "a tracking edit makes nothing suspect");
    let b = r.read().block(&Scope::Canon, &id(&dec)).unwrap();
    assert_eq!(b.field_str("built"), Some("0.38.0"));
    assert_eq!(b.field_str("status"), Some("accepted"));
    assert_eq!(b.field_str("statement"), Some("Ship weekly"));

    // the history keeps both versions, after a reopen and after a reindex
    let check = |r: &Rdy| {
        let h = r.read().history(&Scope::Canon, &id(&dec), 10).unwrap();
        assert!(h.entries.len() >= 2);
        assert_eq!(
            h.entries[0].justification,
            Some(JustificationInfo::Editorial {
                class: "clarify".into(),
                reason: "shipped in 0.38.0".into()
            })
        );
        assert_eq!(built_in(r, &Scope::Canon, &dec).as_deref(), Some("0.38.0"));
        rdy_qa::invariants::check(r, &Scope::Canon).unwrap();
    };
    check(&r);
    drop(r);
    let mut r = Rdy::open(dir.path()).unwrap();
    check(&r);
    r.reindex().unwrap();
    check(&r);
    // a block the pack lists without built is untouched: unversioned input stays absent
    assert!(matches!(
        r.read()
            .block(&Scope::Canon, &id(&section))
            .unwrap()
            .fields
            .get("built"),
        None::<&FieldValue>
    ));
}

fn created_id(res: &WriteResult, key: &str) -> BlockId {
    id(&res.created[key])
}
