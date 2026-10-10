//! acc1: rdy_accept takes write suggestions in bulk (FW-EN3); rdy_overview, rdy_schema and the
//! server instructions carry the entity surfaces (FW-EN1, FW-EN4, FW-CV1 names).
use rdy_engine::Scope;
use rdy_engine::engine::Local;
use rdy_graph::EdgeKind;
use rdy_mcp::entity_tools::rdy_accept;
use rdy_mcp::server::INSTRUCTIONS;
use rdy_mcp::tools::{
    EDIT_LOOP_DOC, ENTITY_RULE, McpState, rdy_overview, rdy_schema, rdy_session, rdy_write,
};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::sync::Arc;

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// Doc `ops`, section Summary, entity Ledger service (alias ledger), landed; an MCP state with a
/// writer session. Returns (dir, state, section, entity).
fn books() -> (tempfile::TempDir, McpState, String, String) {
    let (d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service", "aliases": ["ledger"]}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:a", "run": "a1", "role": "writer"})),
    )
    .unwrap();
    (d, st, w.created["$s"].clone(), w.created["$e"].clone())
}

fn claim(st: &mut McpState, s: &str, body: &str) -> Value {
    rdy_write(st, p(json!({"ops": [{"op": "create", "type": "claim", "id": "$c", "parent": s, "body": body}]}))).unwrap()
}

fn links(st: &McpState, id: &str, kind: &str) -> Vec<String> {
    let rs = st.engine.read();
    let g = rs.graph(&Scope::View(st.view.clone())).unwrap();
    g.outgoing(&BlockId::parse(id).unwrap())
        .iter()
        .filter(|e| e.kind == EdgeKind::Link(kind.into()))
        .map(|e| e.to.to_string())
        .collect()
}

#[test]
fn acc1_write_suggestions_are_accepted_in_one_call() {
    let (_d, mut st, s, e) = books();
    let v = claim(&mut st, &s, "The ledger is append-only.");
    let c = v["created"]["$c"].as_str().unwrap().to_string();
    assert_eq!(v["suggestions"].as_array().map(Vec::len), Some(1), "{v}");
    let sugg = v["suggestions"].clone();
    let a = rdy_accept(&mut st, p(json!({"suggestions": sugg}))).unwrap();
    assert_eq!(a["accepted"], json!({"links": 1, "entities": 0}), "{a}");
    assert!(
        a["consequences"].is_array() && a["suggestions"].is_array(),
        "an accept is a write like any other: {a}"
    );
    assert_eq!(links(&st, &c, "describes"), vec![e]);
}

#[test]
fn acc1_duplicates_collapse_and_a_second_accept_has_nothing_to_do() {
    let (_d, mut st, s, e) = books();
    let v = claim(&mut st, &s, "The ledger is append-only.");
    let c = v["created"]["$c"].as_str().unwrap().to_string();
    let one = v["suggestions"][0].clone();
    let a = rdy_accept(
        &mut st,
        p(json!({"suggestions": [one.clone(), one.clone()]})),
    )
    .unwrap();
    assert_eq!(a["accepted"]["links"], 1, "{a}");
    assert_eq!(a["skipped"][0]["index"], 1, "{a}");
    assert_eq!(links(&st, &c, "describes"), vec![e]);
    let again = rdy_accept(&mut st, p(json!({"suggestions": [one]}))).unwrap_err();
    assert_eq!(
        (again.code, again.message.as_str()),
        (
            "E_BAD_INPUT",
            "nothing to accept: every link is already there"
        )
    );
}

#[test]
fn acc1_an_entity_suggestion_creates_the_entity_and_links_where_it_recurs() {
    let (_d, mut st, s, _e) = books();
    let mut claims = vec![];
    let mut last = Value::Null;
    for body in [
        "Set `max_conn` low.",
        "Raise `max_conn` at peak.",
        "`max_conn` caps the pool.",
    ] {
        last = claim(&mut st, &s, body);
        claims.push(last["created"]["$c"].as_str().unwrap().to_string());
    }
    let mut ent = last["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["kind"] == "entity")
        .cloned()
        .expect("an entity suggestion");
    assert_eq!(ent["literal"], "max_conn");
    ent["entity_kind"] = json!("config key");
    let a = rdy_accept(&mut st, p(json!({"suggestions": [ent], "parent": s}))).unwrap();
    assert_eq!(a["accepted"], json!({"links": 3, "entities": 1}), "{a}");
    let new = a["created"]["$en0"].as_str().unwrap().to_string();
    {
        let rs = st.engine.read();
        let b = rs
            .block(
                &Scope::View(st.view.clone()),
                &BlockId::parse(&new).unwrap(),
            )
            .unwrap();
        assert_eq!(
            (
                b.block_type.as_str(),
                b.field_str("name"),
                b.field_str("kind")
            ),
            ("entity", Some("max_conn"), Some("config key"))
        );
    }
    for c in claims {
        assert_eq!(links(&st, &c, "describes"), vec![new.clone()]);
    }
}

#[test]
fn acc1_bad_input_is_refused_and_nothing_is_written() {
    let (_d, mut st, s, e) = books();
    let e1 = rdy_accept(&mut st, p(json!({"suggestions": []}))).unwrap_err();
    assert_eq!(
        (e1.code, e1.message.as_str()),
        ("E_BAD_INPUT", "no suggestions to accept")
    );
    let ent =
        json!({"kind": "entity", "id": "sg-0", "literal": "max_conn", "blocks": [], "count": 3});
    let e2 = rdy_accept(&mut st, p(json!({"suggestions": [ent]}))).unwrap_err();
    assert_eq!(
        (e2.code, e2.message.as_str()),
        ("E_BAD_INPUT", "suggestion 0: a new entity needs a parent")
    );
    let bad = json!({"kind": "link", "block": s, "link": "depends_on", "target": e});
    let e3 = rdy_accept(&mut st, p(json!({"suggestions": [bad]}))).unwrap_err();
    assert_eq!(
        (e3.code, e3.message.as_str()),
        (
            "E_BAD_INPUT",
            "suggestion 0: link depends_on is not describes or affects"
        )
    );
    let e4 = rdy_accept(&mut st, p(json!({"suggestions": [{"kind": "nonsense"}]}))).unwrap_err();
    assert_eq!(e4.code, "E_BAD_INPUT");
    assert!(e4.message.starts_with("suggestion 0: "), "{e4}");
    let many: Vec<Value> = (0..501)
        .map(|_| json!({"kind": "link", "block": s, "link": "describes", "target": e}))
        .collect();
    assert_eq!(
        rdy_accept(&mut st, p(json!({"suggestions": many})))
            .unwrap_err()
            .code,
        "E_LIMIT"
    );
}

#[test]
fn acc1_overview_schema_and_instructions_carry_the_entity_surfaces() {
    let (_d, mut st, _s, _e) = books();
    let ov = rdy_overview(&mut st, p(json!({}))).unwrap();
    assert_eq!(
        (
            ov["coverage"]["scope"].as_str(),
            ov["coverage"]["entities"].as_u64()
        ),
        (Some("all"), Some(1)),
        "{ov}"
    );
    assert!(
        ov.get("health").is_none(),
        "FW-CV1: the name is coverage: {ov}"
    );
    let sc = rdy_schema(&mut st, p(json!({}))).unwrap();
    assert_eq!(
        sc["entities"]["types"],
        json!(["actor", "component", "entity", "interface", "term", "value"])
    );
    assert_eq!(sc["entities"]["rule"], ENTITY_RULE);
    assert!(
        INSTRUCTIONS
            .contains("Search a thing's name, key or alias with rdy_search to get its fact sheet.")
    );
    assert!(
        INSTRUCTIONS.starts_with("rdy keeps decisions and documentation consistent while agents change them. Call rdy_session first, then rdy_overview."),
        "{INSTRUCTIONS}"
    );
    assert!(
        !INSTRUCTIONS.contains("version control for design docs"),
        "{INSTRUCTIONS}"
    );
    assert!(EDIT_LOOP_DOC.contains("pass the ones you want to rdy_accept"));
}

#[test]
fn acc1_rdy_accept_is_a_tool_over_stdio() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let names: Vec<String> = c
        .tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"rdy_accept".to_string()), "{names:?}");
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "a2", "role": "writer"})
        )
        .code()
        .is_none()
    );
    assert_eq!(
        c.call("rdy_accept", json!({"suggestions": []})).code(),
        Some("E_BAD_INPUT")
    );
    c.alive();
}

/// Final review fix round 1: an entity item may carry a body, so a term (body required) can be
/// created; without one the write refuses it as before.
#[test]
fn acc1_an_entity_item_can_create_a_term_with_its_body() {
    let (_d, mut st, s, _e) = books();
    let v = claim(&mut st, &s, "Set `max_conn` low.");
    let c = v["created"]["$c"].as_str().unwrap().to_string();
    let item = json!({"kind": "entity", "literal": "max_conn", "blocks": [c], "type": "term", "body": "The most pool connections."});
    let a = rdy_accept(&mut st, p(json!({"suggestions": [item], "parent": s}))).unwrap();
    assert_eq!(a["accepted"], json!({"links": 1, "entities": 1}), "{a}");
    let new = a["created"]["$en0"].as_str().unwrap().to_string();
    {
        let rs = st.engine.read();
        let b = rs
            .block(
                &Scope::View(st.view.clone()),
                &BlockId::parse(&new).unwrap(),
            )
            .unwrap();
        assert_eq!(
            (b.block_type.as_str(), b.field_str("name"), b.body.as_str()),
            ("term", Some("max_conn"), "The most pool connections.")
        );
    }
    assert_eq!(links(&st, &c, "describes"), vec![new]);
}

/// Final review fix round 1: a link item must target an entity.
#[test]
fn acc1_a_link_must_target_an_entity() {
    let (_d, mut st, s, _e) = books();
    let v = claim(&mut st, &s, "Nothing named here.");
    let c = v["created"]["$c"].as_str().unwrap().to_string();
    let bad = json!({"kind": "link", "block": c, "link": "describes", "target": s});
    let e = rdy_accept(&mut st, p(json!({"suggestions": [bad]}))).unwrap_err();
    assert_eq!(
        (e.code, e.message),
        (
            "E_BAD_INPUT",
            format!("suggestion 0: {s} is a section, not an entity")
        )
    );
    assert!(links(&st, &c, "describes").is_empty());
}

/// Final review fix round 1: a deleted block in an entity item is skipped as missing, not as a
/// block that cannot link.
#[test]
fn acc1_a_deleted_block_in_an_entity_item_is_skipped_as_missing() {
    let (_d, mut st, s, _e) = books();
    let v = claim(&mut st, &s, "Set `max_conn` low.");
    let c = v["created"]["$c"].as_str().unwrap().to_string();
    rdy_write(&mut st, p(json!({"ops": [{"op": "delete", "id": c}]}))).unwrap();
    let item = json!({"kind": "entity", "literal": "max_conn", "blocks": [c]});
    let a = rdy_accept(&mut st, p(json!({"suggestions": [item], "parent": s}))).unwrap();
    assert_eq!(a["accepted"], json!({"links": 0, "entities": 1}), "{a}");
    assert_eq!(
        a["skipped"],
        json!([{"index": 0, "reason": format!("no block {c} in view {}", st.view)}])
    );
}

#[test]
fn msw18_a_stale_link_item_is_skipped_and_the_rest_are_accepted() {
    use rdy_engine::accept::AcceptItem;
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "The ledger is append-only."}
    ])).unwrap();
    let (s, e, c) = (
        w.created["$s"].clone(),
        w.created["$e"].clone(),
        w.created["$c"].clone(),
    );
    let dec = r
        .decide(
            &writer(),
            "work",
            rdy_engine::DecisionInput {
                parent: s,
                statement: "Keep the ledger".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap(); // the gate accepts dec: it is frozen now
    let items = vec![
        AcceptItem::Link {
            block: dec.clone(),
            link: "affects".into(),
            target: e.clone(),
        },
        AcceptItem::Link {
            block: c.clone(),
            link: "describes".into(),
            target: e.clone(),
        },
    ];
    let a = r.accept(&writer(), "v", items, None, None, None).unwrap();
    assert_eq!(a.accepted.links, 1);
    assert_eq!(a.skipped.len(), 1, "{:?}", a.skipped);
    assert_eq!(a.skipped[0].index, 0);
    assert!(
        a.skipped[0].reason
            == format!("{dec} is an accepted or rejected decision and cannot change"),
        "{:?}",
        a.skipped
    );
    let cb = r
        .read()
        .block(&Scope::View("v".into()), &BlockId::parse(&c).unwrap())
        .unwrap();
    assert!(
        cb.links
            .get("describes")
            .is_some_and(|ls| ls.iter().any(|l| l.target.to_string() == e))
    );
}

#[test]
fn msw18_every_item_skipped_on_a_frozen_decision_says_why() {
    use rdy_engine::accept::AcceptItem;
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service"}}
    ])).unwrap();
    let (s, e) = (w.created["$s"].clone(), w.created["$e"].clone());
    let dec = r
        .decide(
            &writer(),
            "work",
            rdy_engine::DecisionInput {
                parent: s,
                statement: "Keep the ledger".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap(); // the gate accepts dec: it is frozen now
    let items = vec![AcceptItem::Link {
        block: dec.clone(),
        link: "affects".into(),
        target: e,
    }];
    let err = r
        .accept(&writer(), "v", items, None, None, None)
        .unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            "E_BAD_INPUT",
            format!(
                "nothing to accept: every suggestion was skipped; suggestion 0: {dec} is an accepted or rejected decision and cannot change"
            )
            .as_str(),
            "re-run the suggestions and accept the ones that still apply"
        )
    );
}

#[test]
fn msw18_every_item_skipped_on_a_deleted_block_says_why() {
    let (_d, mut st, s, e) = books();
    let v = claim(&mut st, &s, "The ledger is append-only.");
    let c = v["created"]["$c"].as_str().unwrap().to_string();
    rdy_write(&mut st, p(json!({"ops": [{"op": "delete", "id": c}]}))).unwrap();
    let item = json!({"kind": "link", "block": c, "link": "describes", "target": e});
    let err = rdy_accept(&mut st, p(json!({"suggestions": [item]}))).unwrap_err();
    assert_eq!(
        (err.code, err.message, err.fix),
        (
            "E_BAD_INPUT",
            format!(
                "nothing to accept: every suggestion was skipped; suggestion 0: no block {c} in view {}",
                st.view
            ),
            "re-run the suggestions and accept the ones that still apply".to_string()
        )
    );
}

/// Final review M13: the all-skipped refusal lists the first skips and counts the rest, so a
/// batch of up to MAX_ACCEPT items never gives an error of tens of KB.
#[test]
fn msw18_every_item_skipped_lists_the_first_ten_and_counts_the_rest() {
    let (_d, mut st, s, e) = books();
    let v = claim(&mut st, &s, "The ledger is append-only.");
    let c = v["created"]["$c"].as_str().unwrap().to_string();
    rdy_write(&mut st, p(json!({"ops": [{"op": "delete", "id": c}]}))).unwrap();
    let item = json!({"kind": "link", "block": c, "link": "describes", "target": e});
    let items: Vec<_> = (0..14).map(|_| item.clone()).collect();
    let err = rdy_accept(&mut st, p(json!({"suggestions": items}))).unwrap_err();
    assert_eq!(err.code, "E_BAD_INPUT");
    assert_eq!(
        err.message.matches("suggestion ").count(),
        11,
        "{}",
        err.message
    );
    assert!(
        err.message.contains("suggestion 9: no block"),
        "{}",
        err.message
    );
    assert!(!err.message.contains("suggestion 10:"), "{}", err.message);
    assert!(err.message.ends_with("; and 4 more"), "{}", err.message);
}

#[test]
fn msw18_every_item_already_linked_keeps_the_already_there_text() {
    let (_d, mut st, s, _e) = books();
    let v = claim(&mut st, &s, "The ledger is append-only.");
    let one = v["suggestions"][0].clone();
    rdy_accept(&mut st, p(json!({"suggestions": [one.clone()]}))).unwrap();
    let err = rdy_accept(&mut st, p(json!({"suggestions": [one.clone(), one]}))).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            "E_BAD_INPUT",
            "nothing to accept: every link is already there",
            "nothing to do"
        )
    );
}
