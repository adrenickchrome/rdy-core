//! gcov1: graph coverage (FW-EN4, FW-EN1a; named by FW-CV1): normative facts and all content tied
//! to an entity and entity mentions linked (the Grounded measures), docs nothing links to (the
//! Connected measure), and the open entity kinds with the near-duplicates rdy suggests merging.
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Doc `ops` (section Summary): entities Ledger service (kind service, alias ledger), Billing
/// (kind Services), Auth (kind protocol); claim c1 describes the ledger; claim c2 names Billing;
/// requirement r1 names Auth; prose p1 names the Ledger service. Doc `home` holds prose that
/// references c1. Doc `orphan` holds one claim. Landed.
fn world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service", "kind": "service", "aliases": ["ledger"]}},
        {"op": "create", "type": "entity", "id": "$f", "parent": "$s", "fields": {"name": "Billing", "kind": "Services"}},
        {"op": "create", "type": "entity", "id": "$g", "parent": "$s", "fields": {"name": "Auth", "kind": "protocol"}},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "The ledger is append-only.", "links": {"describes": [{"target": "$e"}]}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Billing runs nightly."},
        {"op": "create", "type": "requirement", "id": "$r1", "parent": "$s", "body": "Auth tokens expire."},
        {"op": "create", "type": "prose", "id": "$p1", "parent": "$s", "body": "The Ledger service is described above."},
        {"op": "create", "type": "doc", "id": "$i", "fields": {"name": "home", "title": "Home", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$i", "body": "See {{$c1}}."},
        {"op": "create", "type": "doc", "id": "$z", "fields": {"name": "orphan", "title": "Orphan", "doc_type": "overview"}},
        {"op": "create", "type": "claim", "parent": "$z", "body": "Nothing links here."}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    w.created
}

fn coverage(r: &Rdy, covered: Option<&[&str]>) -> Value {
    coverage_in(r, &Scope::Canon, covered)
}

fn coverage_in(r: &Rdy, scope: &Scope, covered: Option<&[&str]>) -> Value {
    let ids = covered.map(|c| c.iter().map(|i| BlockId::parse(i).unwrap()).collect());
    serde_json::to_value(r.read().graph_coverage(scope, ids.as_ref()).unwrap()).unwrap()
}

#[test]
fn gcov1_the_whole_scope() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    // content: c1, c2, r1, p1, the index prose and the orphan claim (6); facts: the four claims
    // and requirements; only c1 is tied
    assert_eq!(
        coverage(&r, None),
        json!({
            "scope": "all",
            "grounded": {
                "facts": 4, "facts_tied": 1, "facts_tied_pct": 25,
                "content": 6, "content_tied": 1, "content_tied_pct": 16,
                "mentions": 4, "mentions_linked": 1, "mentions_linked_pct": 25
            },
            "connected": {
                "docs": 3,
                "unlinked_docs": [{"id": c["$i"], "path": "home"}, {"id": c["$z"], "path": "orphan"}]
            },
            "entities": 3,
            "kinds": [{"kind": "Services", "entities": 1}, {"kind": "protocol", "entities": 1}, {"kind": "service", "entities": 1}],
            "similar_kinds": [["Services", "service"]]
        })
    );
}

#[test]
fn gcov1_what_a_briefing_covers() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    assert_eq!(
        coverage(
            &r,
            Some(&[c["$c2"].as_str(), c["$r1"].as_str(), c["$p1"].as_str()])
        ),
        json!({
            "scope": "covered",
            "grounded": {
                "facts": 2, "facts_tied": 0, "facts_tied_pct": 0,
                "content": 3, "content_tied": 0, "content_tied_pct": 0,
                "mentions": 3, "mentions_linked": 0, "mentions_linked_pct": 0
            },
            "connected": {"docs": 0, "unlinked_docs": []},
            "entities": 0, "kinds": [], "similar_kinds": []
        })
    );
}

#[test]
fn gcov1_a_link_to_a_deleted_entity_does_not_tie_a_fact() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c["$c2"], "links": {"describes": [{"target": c["$f"]}]}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let h = coverage(&r, None);
    assert_eq!(
        (
            h["grounded"]["facts_tied"].as_u64(),
            h["grounded"]["content_tied"].as_u64()
        ),
        (Some(2), Some(2)),
        "{h}"
    );
    // canon will not take a deletion that leaves a link dangling (E_DANGLING_REF), but the view
    // that holds it can be read
    write(&mut r, "work", json!([{"op": "delete", "id": c["$f"]}])).unwrap();
    let h = coverage_in(&r, &Scope::View("work".into()), None);
    assert_eq!(
        (
            h["grounded"]["facts_tied"].as_u64(),
            h["grounded"]["content_tied"].as_u64(),
            h["entities"].as_u64()
        ),
        (Some(1), Some(1), Some(2)),
        "{h}"
    );
}

#[test]
fn gcov1_prose_counts_as_content_but_not_as_a_fact() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c["$p1"], "links": {"describes": [{"target": c["$e"]}]}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let h = coverage(&r, None);
    assert_eq!(
        (
            h["grounded"]["facts_tied"].as_u64(),
            h["grounded"]["content_tied"].as_u64(),
            h["grounded"]["mentions_linked"].as_u64()
        ),
        (Some(1), Some(2), Some(2)),
        "{h}"
    );
}

#[test]
fn gcov1_an_empty_project_has_no_shares() {
    let (_d, r) = project();
    let h = coverage(&r, None);
    let g = &h["grounded"];
    assert_eq!(
        (
            g["facts"].as_u64(),
            g["facts_tied_pct"].clone(),
            g["content_tied_pct"].clone(),
            g["mentions_linked_pct"].clone()
        ),
        (Some(0), Value::Null, Value::Null, Value::Null)
    );
}

/// Final review fix round 1 guard: whole-scope coverage is memoised per build; a write gives a
/// new build, so the next reading counts it.
#[test]
fn gcov1_whole_scope_coverage_follows_each_write() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let scope = Scope::View("work".into());
    let before = coverage_in(&r, &scope, None);
    assert_eq!(
        before,
        coverage_in(&r, &scope, None),
        "a second reading is the same"
    );
    write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "parent": c["$s"], "body": "Billing closes at night.", "links": {"describes": [{"target": c["$f"]}]}}
    ]))
    .unwrap();
    let after = coverage_in(&r, &scope, None);
    assert_eq!(
        (
            after["grounded"]["facts"].as_u64(),
            after["grounded"]["facts_tied"].as_u64()
        ),
        (
            before["grounded"]["facts"].as_u64().map(|n| n + 1),
            before["grounded"]["facts_tied"].as_u64().map(|n| n + 1)
        ),
        "{after}"
    );
}
