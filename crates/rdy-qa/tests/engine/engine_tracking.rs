//! trk1: tracking fields stay writable on an accepted decision, with an editorial reason and a
//! history; the ruling stays frozen; a tracking edit makes nothing suspect; the surfaces.
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_model::{BlockId, EditorialClass, Justification};
use rdy_qa::fixtures::*;
use serde_json::{from_value, json};
use std::sync::Arc;

const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
      trigger: { kind: str }
    tracking: [disposition, trigger]
";

fn trk_project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy/config.yaml"), "pack: trk\n").unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    (dir, r)
}
fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}
/// Canon: a seed doc whose section holds an accepted decision (disposition "M2d1") and a claim
/// whose decided_by link pins it. Returns (decision, claim).
fn accepted(r: &mut Rdy) -> (String, String) {
    let (_doc, section, _c) = seed_doc(r, "seed");
    let d = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: section.clone(),
                statement: "Ship weekly".into(),
                fields: [("disposition".to_string(), json!("M2d1"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap();
    let dec = d.created["$decision"].clone();
    let res = write(r, "seed", json!([{"op": "create", "type": "claim", "id": "$c", "parent": section, "body": "We ship weekly.", "links": {"decided_by": [{"target": dec}]}}])).unwrap();
    land(r, "seed").unwrap();
    assert_eq!(
        r.read()
            .block(&Scope::Canon, &id(&dec))
            .unwrap()
            .field_str("status"),
        Some("accepted")
    );
    (dec, res.created["$c"].clone())
}
fn update(
    r: &mut Rdy,
    view: &str,
    dec: &str,
    fields: serde_json::Value,
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
fn trk1_a_tracking_field_changes_after_acceptance_with_an_editorial_reason_and_keeps_history() {
    let (_d, mut r) = trk_project();
    let (dec, _claim) = accepted(&mut r);
    let e = update(&mut r, "reg", &dec, json!({"disposition": "M2d2"}), None).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.contains("editorial reason"), "{}", e.message);
    assert_eq!(
        update(
            &mut r,
            "reg",
            &dec,
            json!({"disposition": "M2d2"}),
            Some("  ")
        )
        .unwrap_err()
        .code,
        E_BAD_INPUT
    );
    update(
        &mut r,
        "reg",
        &dec,
        json!({"disposition": "M2d2", "trigger": "after M2c3"}),
        Some("re-sliced into M2d2"),
    )
    .unwrap();
    land(&mut r, "reg").unwrap();
    let b = r.read().block(&Scope::Canon, &id(&dec)).unwrap();
    assert_eq!(b.field_str("disposition"), Some("M2d2"));
    assert_eq!(b.field_str("trigger"), Some("after M2c3"));
    assert_eq!(
        b.field_str("status"),
        Some("accepted"),
        "still accepted: the ruling did not move"
    );
    assert_eq!(b.field_str("statement"), Some("Ship weekly"));
    let h = r.read().history(&Scope::Canon, &id(&dec), 10).unwrap();
    assert!(
        h.entries.len() >= 2,
        "the first version and the tracking edit are both kept"
    );
    assert_eq!(
        h.entries[0].justification,
        Some(JustificationInfo::Editorial {
            class: "clarify".into(),
            reason: "re-sliced into M2d2".into()
        })
    );
    rdy_qa::invariants::check(&r, &Scope::Canon).unwrap();
}

#[test]
fn trk1_the_ruling_stays_frozen() {
    let (_d, mut r) = trk_project();
    let (dec, claim) = accepted(&mut r);
    let why = Some("tidy");
    for fields in [
        json!({"statement": "Ship daily"}),
        json!({"statement": "Ship daily", "disposition": "M2d2"}),
        json!({"kind": "process"}),
        json!({"kind": "process", "disposition": "M2d2"}),
    ] {
        assert_eq!(
            update(&mut r, "reg", &dec, fields.clone(), why)
                .unwrap_err()
                .code,
            E_FROZEN,
            "{fields}"
        );
    }
    let links = r.write(&writer(), WriteRequest {
        view: "reg".into(),
        ops: from_value(json!([{"op": "update", "id": dec, "fields": {"disposition": "M2d2"}, "links": {"affects": [{"target": claim}]}}])).unwrap(),
        justification: Some(Justification::Editorial { class: EditorialClass::Clarify, reason: "x".into() }),
        message: String::new(),
    });
    assert_eq!(links.unwrap_err().code, E_FROZEN, "links are the ruling");
    let del = r.write(
        &writer(),
        WriteRequest {
            view: "reg".into(),
            ops: from_value(json!([{"op": "delete", "id": dec}])).unwrap(),
            justification: None,
            message: String::new(),
        },
    );
    assert_eq!(del.unwrap_err().code, E_FROZEN);
}

#[test]
fn trk1_a_tracking_edit_makes_nothing_suspect() {
    let (_d, mut r) = trk_project();
    let (dec, claim) = accepted(&mut r);
    let pinned = |r: &Rdy| {
        r.read()
            .suspects(&Scope::Canon)
            .unwrap()
            .iter()
            .any(|s| s.block == id(&claim))
    };
    assert!(!pinned(&r));
    update(
        &mut r,
        "reg",
        &dec,
        json!({"disposition": "M2d2"}),
        Some("re-sliced"),
    )
    .unwrap();
    land(&mut r, "reg").unwrap();
    assert!(!pinned(&r), "the claim's pin on the decision still matches");
    // the same check after a rebuild (the graph is built, not carried)
    r.invalidate();
    assert!(!pinned(&r));
}

#[test]
fn trk1_tracking_fields_show_in_rdy_schema_and_show() {
    let (_d, mut r) = trk_project();
    let (dec, claim) = accepted(&mut r);
    let ctx = r.read().context(&Scope::Canon, &id(&dec)).unwrap();
    assert_eq!(
        ctx.tracking,
        vec!["disposition".to_string(), "trigger".to_string()]
    );
    let v = serde_json::to_value(r.read().context(&Scope::Canon, &id(&claim)).unwrap()).unwrap();
    assert!(
        v.get("tracking").is_none(),
        "a type without tracking fields: no key"
    );
    let mut st = rdy_mcp::tools::McpState::new(Arc::new(Local::new(r)));
    rdy_mcp::tools::rdy_session(
        &mut st,
        from_value(json!({"actor": "agent:a", "run": "ra", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let s = rdy_mcp::tools::rdy_schema(
        &mut st,
        from_value(json!({"block_type": "decision"})).unwrap(),
    )
    .unwrap();
    assert_eq!(s["tracking"], json!(["disposition", "trigger"]));
    let s =
        rdy_mcp::tools::rdy_schema(&mut st, from_value(json!({"block_type": "claim"})).unwrap())
            .unwrap();
    assert!(s.get("tracking").is_none());
    let all = rdy_mcp::tools::rdy_schema(&mut st, from_value(json!({})).unwrap()).unwrap();
    let d = all["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["type"] == "decision")
        .unwrap();
    assert_eq!(d["tracking"], json!(["disposition", "trigger"]));
}

/// Final-review fix round 1: `rdy edit`'s write (`edit_block_with`, which `edit_yaml` calls)
/// carries a reason as an editorial `clarify` justification, so a person can move a tracking
/// field from the CLI; without one, the refusal names `rdy edit --reason`.
#[test]
fn trk1_an_edit_with_a_reason_moves_a_tracking_field() {
    let (_d, mut r) = trk_project();
    let (dec, _claim) = accepted(&mut r);
    let mut edited = r.read().block(&Scope::Canon, &id(&dec)).unwrap();
    edited.fields.insert(
        "disposition".into(),
        rdy_model::FieldValue::Str("M2d2".into()),
    );
    let e = r
        .edit_block_with(&writer(), "reg", edited.clone(), None, None)
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.contains("editorial reason"), "{}", e.message);
    assert!(
        e.fix.contains("rdy edit") && e.fix.contains("--reason"),
        "{}",
        e.fix
    );
    // a blank reason is no reason
    assert_eq!(
        r.edit_block_with(&writer(), "reg", edited.clone(), None, Some("  "))
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    r.edit_block_with(&writer(), "reg", edited, None, Some("re-sliced into M2d2"))
        .unwrap();
    land(&mut r, "reg").unwrap();
    let b = r.read().block(&Scope::Canon, &id(&dec)).unwrap();
    assert_eq!(b.field_str("disposition"), Some("M2d2"));
    let h = r.read().history(&Scope::Canon, &id(&dec), 10).unwrap();
    assert_eq!(
        h.entries[0].justification,
        Some(JustificationInfo::Editorial {
            class: "clarify".into(),
            reason: "re-sliced into M2d2".into()
        })
    );
}
