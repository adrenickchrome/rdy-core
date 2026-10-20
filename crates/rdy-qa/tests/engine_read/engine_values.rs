//! vfu1: a value's references go suspect when it changes, with the follow-up confirm: their text
//! updates by itself, but the advice around it may not (FW-EN2). Summaries show the value.
use rdy_engine::brief::{Coverage, FollowUp};
use rdy_engine::consequence::ConsequenceKind;
use rdy_engine::*;
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::*;
use serde_json::json;

/// Doc `limits`, section Summary: value Request timeout = 5 ms, term Pool, and requirement R that
/// references both. Landed. Returns (value, term, requirement).
fn limits(r: &mut Rdy) -> (String, String, String) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "limits", "title": "Limits", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "value", "id": "$v", "parent": "$s", "fields": {"name": "Request timeout", "value": "5", "unit": "ms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "body": "The set of open connections.", "fields": {"name": "Pool"}},
        {"op": "create", "type": "requirement", "id": "$r", "parent": "$s", "body": "Requests time out after {{$v}}; the {{$t}} is then drained."}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    let c = |k: &str| w.created[k].clone();
    (c("$v"), c("$t"), c("$r"))
}

fn follow(r: &Rdy, w: &WriteResult, block: &str) -> (ConsequenceKind, FollowUp) {
    let change: Hash = w.change.parse().unwrap();
    let cs = r
        .read()
        .consequences(&Scope::View("work".into()), &change, None)
        .unwrap();
    let c = cs
        .items
        .iter()
        .find(|c| c.block == block)
        .unwrap_or_else(|| panic!("{block} not in {:?}", cs.items));
    (c.kind, c.follow_up)
}

#[test]
fn vfu1_a_changed_value_asks_its_references_to_confirm() {
    let (_d, mut r) = project();
    let (v, t, req) = limits(&mut r);
    let w = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": v, "fields": {"value": "7"}}]),
    )
    .unwrap();
    assert_eq!(
        follow(&r, &w, &req),
        (ConsequenceKind::Suspect, FollowUp::Confirm)
    );
    // guard: a changed term whose removed text the block does not state asks it to confirm
    // (D-CFC6V: the follow-up comes from changed literals, not the type)
    let w = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": t, "body": "The open connections."}]),
    )
    .unwrap();
    assert_eq!(
        follow(&r, &w, &req),
        (ConsequenceKind::Suspect, FollowUp::Confirm)
    );
}

#[test]
fn vfu1_a_deleted_value_asks_for_an_update() {
    let (_d, mut r) = project();
    let (v, _t, req) = limits(&mut r);
    let w = write(&mut r, "work", json!([{"op": "delete", "id": v}])).unwrap();
    assert_eq!(follow(&r, &w, &req).1, FollowUp::Update);
}

#[test]
fn vfu1_a_briefing_on_a_value_says_confirm() {
    let (_d, mut r) = project();
    let (v, _t, req) = limits(&mut r);
    let b = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![v],
                path: None,
            },
            None,
        )
        .unwrap();
    let got: Vec<(String, FollowUp)> = b
        .will_go_stale
        .iter()
        .map(|w| (w.block.clone(), w.follow_up))
        .collect();
    assert_eq!(got, vec![(req, FollowUp::Confirm)]);
}

#[test]
fn vfu1_summaries_show_a_referenced_value_as_its_value() {
    let (_d, mut r) = project();
    let (v, _t, req) = limits(&mut r);
    let hits = r
        .read()
        .impact(&Scope::Canon, &BlockId::parse(&v).unwrap(), 1)
        .unwrap();
    let h = hits.iter().find(|h| h.id == req).unwrap();
    assert_eq!(
        h.summary,
        "Requests time out after 5 ms; the Pool is then drained."
    );
}
