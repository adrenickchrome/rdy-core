//! nb3 (D-YXBXH, D-2593P): an entity's optional `everyday` flag goes through the same write and
//! read tools as any other field. A write to stable content still needs a current briefing and a
//! normal justification; there is no new tool.
use rdy_engine::Rdy;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

/// A stable doc with a section, an entity `Ledger` and a value `Limit`, landed. Returns the
/// entity's and the value's ids.
fn world(r: &mut Rdy) -> (String, String) {
    let w = r
        .write(
            &writer(),
            rdy_engine::WriteRequest {
                view: "work".into(),
                ops: parse_test_ops(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
                    {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S", "lifecycle": "stable"}},
                    {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger"}},
                    {"op": "create", "type": "value", "id": "$v", "parent": "$s", "body": "Most items a level holds.", "fields": {"name": "Limit", "value": "8"}}
                ]))
                .unwrap(),
                justification: Some(rdy_model::Justification::Editorial {
                    class: rdy_model::EditorialClass::Clarify,
                    reason: "seed the test world".into(),
                }),
                message: String::new(),
            },
        )
        .unwrap();
    land(r, "work").unwrap();
    (w.created["$e"].clone(), w.created["$v"].clone())
}

fn agent(c: &mut McpClient) {
    let s = c.call(
        "rdy_session",
        json!({"actor": "agent:e", "run": "e1", "role": "writer"}),
    );
    assert!(s.code().is_none(), "{s:?}");
}

fn token(c: &mut McpClient, id: &str) -> String {
    c.call("rdy_brief", json!({"ids": [id]})).ok()["base"]["token"]
        .as_str()
        .unwrap()
        .to_string()
}

fn unjustified(c: &mut McpClient) -> Vec<Value> {
    let v = c
        .call("rdy_tensions", json!({"kind": "unjustified_change"}))
        .ok();
    v["tensions"].as_array().cloned().unwrap_or_default()
}

#[test]
fn nb3_mcp_writes_and_reads_everyday_through_the_gate() {
    let (dir, mut r) = project();
    let (entity, value) = world(&mut r);
    drop(r);
    let mut c = McpClient::start(dir.path());
    agent(&mut c);

    // stable content: the flag needs a current briefing, as every other field does
    let on = json!([{"op": "update", "id": entity, "fields": {"everyday": true}}]);
    let e = c.call("rdy_write", json!({"ops": on.clone()}));
    assert_eq!(e.code(), Some("E_BRIEF_REQUIRED"), "{e:?}");
    let t = token(&mut c, &entity);
    let w = c.call("rdy_write", json!({"ops": on, "brief": t}));
    assert!(w.code().is_none(), "{w:?}");

    // the round trip: rdy_context reads the stored boolean
    let ctx = c.call("rdy_context", json!({"id": entity})).ok();
    assert_eq!(ctx["fields"]["everyday"], json!(true), "{ctx}");

    // false is stored, not dropped
    let off = json!([{"op": "update", "id": entity, "fields": {"everyday": false}}]);
    let t = token(&mut c, &entity);
    let w = c.call("rdy_write", json!({"ops": off, "brief": t}));
    assert!(w.code().is_none(), "{w:?}");
    let ctx = c.call("rdy_context", json!({"id": entity})).ok();
    assert_eq!(ctx["fields"]["everyday"], json!(false), "{ctx}");

    // the generic field check refuses a wrong type and a typo, and names the field
    let t = token(&mut c, &entity);
    let bad = c.call(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": entity, "fields": {"everyday": "yes"}}], "brief": t}),
    );
    assert_eq!(bad.code(), Some("E_FIELD_KIND"), "{bad:?}");
    let typo = c.call(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": entity, "fields": {"everydy": true}}], "brief": t}),
    );
    assert_eq!(typo.code(), Some("E_FIELD_UNKNOWN"), "{typo:?}");
    assert!(format!("{typo:?}").contains("everyday"), "{typo:?}");

    // a normative defining type: the flag is a change like any other, so it needs the normal
    // justification, and an editorial one clears it
    let on = json!([{"op": "update", "id": value, "fields": {"everyday": true}}]);
    let t = token(&mut c, &value);
    let w = c.call("rdy_write", json!({"ops": on, "brief": t}));
    assert!(w.code().is_none(), "{w:?}");
    let open = unjustified(&mut c);
    assert!(
        open.iter().any(|t| t.to_string().contains(&value)),
        "an unjustified change is raised for the value: {open:?}"
    );
    let again = json!([{"op": "update", "id": value, "fields": {"everyday": false}}]);
    let t = token(&mut c, &value);
    let w = c.call(
        "rdy_write",
        json!({"ops": again, "brief": t, "editorial": {"class": "clarify", "reason": "Not an everyday word."}}),
    );
    assert!(w.code().is_none(), "{w:?}");
    let open = unjustified(&mut c);
    assert!(
        open.iter().all(|t| !t.to_string().contains(&value)),
        "the editorial justification clears it: {open:?}"
    );
    c.alive();
}
