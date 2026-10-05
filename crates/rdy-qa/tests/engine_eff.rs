//! A decision's effective_from (plan M2a1, eff1): declared by the schema, read as recorded_at when
//! absent, set by the proposer while the decision is proposed, and frozen with it.
use rdy_engine::*;
use rdy_model::{BlockId, decision};
use rdy_qa::fixtures::*;
use serde_json::json;

fn log_section(r: &mut Rdy) -> String {
    let res = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]))
    .unwrap();
    res.created["$s"].clone()
}

/// Guard (passes before the change, through Task 3's reader). Review Focus 6: a decision takes
/// effect when recorded.
#[test]
fn eff1_a_decision_takes_effect_when_recorded() {
    let (_d, mut r) = project();
    let parent = log_section(&mut r);
    let res = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent,
                statement: "A".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let b = r
        .block(
            &Scope::View("work".into()),
            &BlockId::parse(&res.created["$decision"]).unwrap(),
        )
        .unwrap();
    assert!(!b.fields.contains_key(decision::EFFECTIVE_FROM));
    assert!(decision::effective_from(&b).is_some());
    assert_eq!(decision::effective_from(&b), b.field_str("recorded_at"));
}

/// Q2d and amendment §3.2: the proposer sets effective_from while the decision is proposed; the
/// reader prefers it to recorded_at; it freezes with the decision.
#[test]
fn eff1_a_proposer_sets_effective_from_and_it_freezes_with_the_decision() {
    let (_d, mut r) = project();
    let parent = log_section(&mut r);
    let view = Scope::View("work".into());
    let early = "2026-09-01T00:00:00Z";
    let res = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "decision", "id": "$b", "parent": parent,
        "fields": {"statement": "B", "status": "proposed", "effective_from": early}}]),
    )
    .unwrap();
    let b = r
        .block(&view, &BlockId::parse(&res.created["$b"]).unwrap())
        .unwrap();
    assert_eq!(decision::effective_from(&b), Some(early), "set on create");
    let res = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent,
                statement: "A".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let id = res.created["$decision"].clone();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id, "fields": {"effective_from": early}}]),
    )
    .unwrap();
    let a = r.block(&view, &BlockId::parse(&id).unwrap()).unwrap();
    assert_eq!(
        decision::effective_from(&a),
        Some(early),
        "set on update, preferred to recorded_at"
    );
    assert_ne!(a.field_str("recorded_at"), Some(early));
    land(&mut r, "work").unwrap();
    let a = r
        .block(&Scope::Canon, &BlockId::parse(&id).unwrap())
        .unwrap();
    assert_eq!(
        (a.field_str("status"), decision::effective_from(&a)),
        (Some("accepted"), Some(early)),
        "the gate leaves it alone"
    );
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id, "fields": {"effective_from": "2026-09-02T00:00:00Z"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_FROZEN", "{e}");
}

/// A value a later rdy recorded validates on this one.
#[test]
fn eff1_the_schema_declares_effective_from() {
    let (_d, r) = project();
    let b = raw(
        "D-AAAAA",
        "decision",
        None,
        "V",
        &[
            ("statement", "A"),
            ("status", "accepted"),
            ("recorded_at", "2026-10-04T10:00:00Z"),
            ("effective_from", "2026-09-01T00:00:00Z"),
        ],
        "",
    );
    let codes: Vec<&str> = rdy_schema::validate_block(r.pack(), &b, None)
        .iter()
        .map(|i| i.code)
        .collect();
    assert!(
        !codes
            .iter()
            .any(|c| matches!(*c, "E_FIELD_UNKNOWN" | "E_FIELD_KIND")),
        "{codes:?}"
    );
}
