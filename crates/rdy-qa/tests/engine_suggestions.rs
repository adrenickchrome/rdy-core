//! sug1, mby1: a write's suggestions (FW-EN3): links to the entities its blocks name, a new
//! entity for a literal that recurs with none behind it; never a gate. A new doc lists the
//! blocks that mention it.
use rdy_engine::consequence::write_and_report;
use rdy_engine::engine::{Local, write_typed};
use rdy_engine::suggest::{Suggestion, suggestion_id};
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::collections::BTreeSet;

/// Doc `ops`, section Summary, entity Ledger service (alias ledger). Landed. Returns (section,
/// entity).
fn books(r: &mut Rdy) -> (String, String) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service", "aliases": ["ledger"]}}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    (w.created["$s"].clone(), w.created["$e"].clone())
}

fn sugg(r: &Rdy, w: &WriteResult) -> (Vec<Suggestion>, usize) {
    r.read()
        .suggestions(&Scope::View("work".into()), &w.change.parse().unwrap())
        .unwrap()
}

fn link(block: &str, kind: &str, target: &str, mention: &str, field: &str) -> Suggestion {
    Suggestion::Link {
        id: suggestion_id(kind, block, target),
        block: block.into(),
        link: kind.into(),
        target: target.into(),
        mention: mention.into(),
        field: field.into(),
    }
}

#[test]
fn sug1_a_claim_that_names_an_entity_gets_a_describes_suggestion() {
    let (_d, mut r) = project();
    let (s, e) = books(&mut r);
    let w = write(&mut r, "work", json!([{"op": "create", "type": "claim", "id": "$c", "parent": s, "body": "The Ledger keeps every entry."}])).unwrap();
    let c = w.created["$c"].clone();
    assert_eq!(
        sugg(&r, &w),
        (vec![link(&c, "describes", &e, "Ledger", "body")], 0)
    );
}

#[test]
fn sug1_no_suggestion_for_a_tied_block_or_the_entity_itself() {
    let (_d, mut r) = project();
    let (s, e) = books(&mut r);
    let w = write(&mut r, "work", json!([{"op": "create", "type": "claim", "parent": s, "body": "The ledger is append-only.", "links": {"describes": [{"target": e}]}}])).unwrap();
    assert_eq!(sugg(&r, &w).0, vec![], "already describes it");
    let w = write(&mut r, "work", json!([{"op": "create", "type": "claim", "parent": s, "body": format!("{{{{{e}}}}} and the ledger agree.")}])).unwrap();
    assert_eq!(sugg(&r, &w).0, vec![], "already references it");
    let w = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": e, "body": "The ledger service keeps the ledger."}]),
    )
    .unwrap();
    assert_eq!(sugg(&r, &w).0, vec![], "an entity never links to itself");
}

#[test]
fn sug1_a_decision_gets_an_affects_suggestion() {
    let (_d, mut r) = project();
    let (s, e) = books(&mut r);
    let w = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s,
                statement: "The Ledger service is the source of truth".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let d = w.created["$decision"].clone();
    assert_eq!(
        sugg(&r, &w).0,
        vec![link(&d, "affects", &e, "Ledger service", "statement")]
    );
}

#[test]
fn sug1_a_literal_that_recurs_with_no_entity_behind_it_suggests_one() {
    let (_d, mut r) = project();
    let (s, _e) = books(&mut r);
    let w1 = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$a", "parent": s, "body": "Set `max_conn` low."},
        {"op": "create", "type": "claim", "id": "$b", "parent": s, "body": "Raise `max_conn` at peak."}
    ]))
    .unwrap();
    assert!(
        sugg(&r, &w1)
            .0
            .iter()
            .all(|x| !matches!(x, Suggestion::Entity { .. })),
        "two blocks are not a pattern yet"
    );
    let w2 = write(&mut r, "work", json!([{"op": "create", "type": "claim", "id": "$c", "parent": s, "body": "`max_conn` caps the pool."}])).unwrap();
    let mut holders = vec![
        w1.created["$a"].clone(),
        w1.created["$b"].clone(),
        w2.created["$c"].clone(),
    ];
    holders.sort();
    let got = sugg(&r, &w2).0;
    let ents: Vec<&Suggestion> = got
        .iter()
        .filter(|x| matches!(x, Suggestion::Entity { .. }))
        .collect();
    assert_eq!(
        ents,
        vec![&Suggestion::Entity {
            id: suggestion_id("entity", "max_conn", ""),
            literal: "max_conn".into(),
            blocks: holders,
            count: 3
        }]
    );
    // once an entity carries that name, the literal is a mention to link instead
    let w3 = write(&mut r, "work", json!([{"op": "create", "type": "entity", "id": "$m", "parent": s, "fields": {"name": "max_conn", "kind": "config key"}}])).unwrap();
    let m = w3.created["$m"].clone();
    let w4 = write(&mut r, "work", json!([{"op": "create", "type": "claim", "id": "$d", "parent": s, "body": "Lower `max_conn` at night."}])).unwrap();
    let got = sugg(&r, &w4).0;
    assert!(
        got.iter().all(|x| !matches!(x, Suggestion::Entity { .. })),
        "{got:?}"
    );
    assert_eq!(
        got,
        vec![link(&w4.created["$d"], "describes", &m, "max_conn", "body")]
    );
}

/// Final review fix round 1: metadata fields are not text. A code block's `lang`, a decision's
/// `kind` and `attested_by`, and an enum field (`priority`) never mention an entity, coverage
/// does not count them, and a literal in them is not counted.
#[test]
fn sug1_metadata_fields_never_mention_an_entity() {
    let (_d, mut r) = project();
    let (s, _e) = books(&mut r);
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "entity", "parent": s, "fields": {"name": "Rust"}},
            {"op": "create", "type": "entity", "parent": s, "fields": {"name": "Product"}},
            {"op": "create", "type": "entity", "parent": s, "fields": {"name": "Founder"}},
            {"op": "create", "type": "entity", "parent": s, "fields": {"name": "Should"}}
        ]),
    )
    .unwrap();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "code", "parent": s, "body": "fn main() {}", "fields": {"lang": "rust"}},
        {"op": "create", "type": "requirement", "parent": s, "body": "Close at night.", "fields": {"priority": "should"}}
    ]))
    .unwrap();
    assert_eq!(
        sugg(&r, &w).0,
        vec![],
        "a code block's lang, a requirement's priority"
    );
    for i in 0..3 {
        let w = r
            .decide(
                &writer(),
                "work",
                DecisionInput {
                    parent: s.clone(),
                    statement: format!("Ship weekly {i}"),
                    kind: Some("product".into()),
                    attested_by: Some("Founder via `ops_bot`".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(sugg(&r, &w).0, vec![], "a decision's kind and attested_by");
    }
    let rs = r.read();
    let scope = Scope::View("work".into());
    assert_eq!(
        rs.lexicon(&scope).unwrap().literal_blocks("ops_bot"),
        vec![]
    );
    assert_eq!(
        rs.graph_coverage(&scope, None).unwrap().grounded.mentions,
        0
    );
}

/// Fix round 1 guard: a literal with no words (`->`) has an empty phrase. No entity can carry
/// that name, so the suggestion could never clear: it is not made.
#[test]
fn sug1_a_literal_with_no_words_never_suggests_an_entity() {
    let (_d, mut r) = project();
    let (s, _e) = books(&mut r);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "parent": s, "body": "Read `->` as maps to."},
        {"op": "create", "type": "claim", "parent": s, "body": "The arrow `->` joins two steps."},
        {"op": "create", "type": "claim", "parent": s, "body": "Each `->` is one hop."}
    ]))
    .unwrap();
    let got = sugg(&r, &w).0;
    assert!(
        got.iter().all(|x| !matches!(x, Suggestion::Entity { .. })),
        "{got:?}"
    );
}

#[test]
fn sug1_every_write_response_carries_suggestions_and_they_never_gate() {
    let (_d, mut r) = project();
    let (s, e) = books(&mut r);
    let eng = Local::new(r);
    let s2 = s.clone();
    let v = write_and_report(&eng, "work", None, move |r| {
        write(r, "work", json!([{"op": "create", "type": "claim", "parent": s2, "body": "The ledger closes daily."}]))
    })
    .unwrap();
    assert_eq!(v["suggestions"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(
        (
            v["suggestions"][0]["kind"].as_str(),
            v["suggestions"][0]["target"].as_str()
        ),
        (Some("link"), Some(e.as_str()))
    );
    assert!(
        v["consequences"].is_array(),
        "suggestions sit beside the consequences: {v}"
    );
    let v = write_and_report(&eng, "work", None, move |r| {
        write(r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "plain", "title": "Plain", "doc_type": "overview"}}]))
    })
    .unwrap();
    assert_eq!(v["suggestions"], json!([]), "{v}");
    // the view lands with its suggestions unanswered
    write_typed(&eng, |r| land(r, "work")).unwrap();
}

#[test]
fn mby1_a_new_doc_lists_the_blocks_that_mention_it() {
    let (_d, mut r) = project();
    let (s, _e) = books(&mut r);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$m", "parent": s, "body": "Every entry is copied to the Archive nightly."},
        {"op": "create", "type": "claim", "parent": s, "body": "Unrelated text."},
        {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "archive", "title": "Archive", "doc_type": "overview"}},
        {"op": "create", "type": "section", "parent": "$a", "fields": {"title": "The Archive"}}
    ]))
    .unwrap();
    let (m, a) = (w.created["$m"].clone(), w.created["$a"].clone());
    let work = Scope::View("work".into());
    let docs: BTreeSet<BlockId> = [BlockId::parse(&a).unwrap(), BlockId::parse(&m).unwrap()].into();
    let (got, omitted) = r.read().mentioned_by(&work, &docs).unwrap();
    assert_eq!(omitted, 0);
    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        json!([{"doc": a, "block": m, "field": "body", "mention": "Archive"}]),
        "a claim id in docs is ignored; the archive's own section does not count"
    );
    // once something links to it, the doc is not new: nothing to list
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": m, "links": {"describes": [{"target": a}]}}]),
    )
    .unwrap();
    assert!(r.read().mentioned_by(&work, &docs).unwrap().0.is_empty());
}
