//! cc5: value disagreement (FW-CH2, D-S46DE): a block that describes a value states a different
//! number in the value's unit.
use rdy_engine::tensions::{TensionKind, TensionSeverity};
use rdy_qa::fixtures::{land, project};
use rdy_qa::tensions::*; // put, put_j, j_typo, created, one_involving, of_kind, has_canon, change_sides
use serde_json::json;

/// Doc `ops`: a stable section Limits holding value "Retry limit" = 5 s, and a draft section
/// Notes; landed. Returns (dir, rdy, stable section, draft section, value).
fn world() -> (
    tempfile::TempDir,
    rdy_engine::Rdy,
    rdy_model::BlockId,
    rdy_model::BlockId,
    rdy_model::BlockId,
) {
    let (d, mut r) = project();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Limits", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$n", "parent": "$d", "fields": {"title": "Notes", "lifecycle": "draft"}},
            {"op": "create", "type": "value", "id": "$v", "parent": "$s", "fields": {"name": "Retry limit", "value": "5", "unit": "s"}}
        ]),
    );
    land(&mut r, "seed").unwrap();
    (
        d,
        r,
        created(&w, "$s"),
        created(&w, "$n"),
        created(&w, "$v"),
    )
}

/// A claim `$y` under `parent` that describes `v`, written briefed and justified as a typo fix,
/// so stable content takes it.
fn claim(
    r: &mut rdy_engine::Rdy,
    view: &str,
    parent: &rdy_model::BlockId,
    v: &rdy_model::BlockId,
    body: &str,
) -> rdy_model::BlockId {
    let w = put_j(
        r,
        view,
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": parent.to_string(),
        "body": body, "links": {"describes": [{"target": v.to_string()}]}}]),
        j_typo(),
    );
    created(&w, "$y")
}

#[test]
fn cc5_a_different_number_in_the_unit_is_a_tension() {
    let (_d, mut r, s, _n, v) = world();
    let y = claim(&mut r, "a", &s, &v, "A job retries for 10 s.");
    let t = one_involving(&r, TensionKind::ValueDisagreement, &y);
    assert!(t.blocks().contains(&v));
    assert_eq!(t.severity, TensionSeverity::Blocking);
    assert!(
        t.summary.contains("states 10 s for Retry limit"),
        "{}",
        t.summary
    );
}

/// guard (pre-flight): a negative test; it fails before the change only by not compiling, and
/// pins the rule's limits once the kind exists.
#[test]
fn cc5_only_a_number_in_the_values_unit_counts() {
    let (_d, mut r, s, _n, v) = world();
    claim(
        &mut r,
        "a",
        &s,
        &v,
        "A job retries for 5 s, at most 10 times, over 2 min.",
    );
    assert!(of_kind(&r, TensionKind::ValueDisagreement).is_empty());
    claim(&mut r, "b", &s, &v, "It waits 5.0 s.");
    assert!(
        of_kind(&r, TensionKind::ValueDisagreement).is_empty(),
        "5.0 is 5"
    );
}

/// guard (pre-flight): a negative test, as above.
#[test]
fn cc5_a_mention_without_a_tie_is_not_a_description() {
    let (_d, mut r, _s, n, _v) = world();
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": n.to_string(),
        "body": "The Retry limit is 10 s."}]),
    );
    assert!(of_kind(&r, TensionKind::ValueDisagreement).is_empty());
}

#[test]
fn cc5_advisory_in_draft_it_persists_in_canon_and_clears_when_the_value_changes() {
    let (_d, mut r, _s, n, v) = world();
    let y = claim(&mut r, "a", &n, &v, "A job retries for 10 s.");
    assert_eq!(
        one_involving(&r, TensionKind::ValueDisagreement, &y).severity,
        TensionSeverity::Advisory
    );
    land(&mut r, "a").unwrap();
    let t = one_involving(&r, TensionKind::ValueDisagreement, &y);
    assert!(
        has_canon(&t) && change_sides(&t).is_empty(),
        "the all-canon form stays open"
    );
    put_j(
        &mut r,
        "b",
        json!([{"op": "update", "id": v.to_string(), "fields": {"value": "10"}}]),
        j_typo(),
    );
    land(&mut r, "b").unwrap();
    assert!(
        of_kind(&r, TensionKind::ValueDisagreement).is_empty(),
        "the trigger stopped holding"
    );
}

/// The carried tension set and a rebuild of the state agree, by id.
fn carried_equals_rebuilt(r: &rdy_engine::Rdy, step: &str) {
    let st = r.read();
    let carried: Vec<_> = st.tensions().unwrap().iter().map(|t| t.id).collect();
    let rebuilt: Vec<_> = st
        .tensions_rebuilt()
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(carried, rebuilt, "{step}");
}

#[test]
fn cc5_the_carried_set_equals_a_rebuild_across_edits_of_the_value_and_the_describer() {
    let (_d, mut r, s, _n, v) = world();
    let y = claim(&mut r, "a", &s, &v, "A job retries for 10 s.");
    carried_equals_rebuilt(&r, "a describer that disagrees");
    assert_eq!(of_kind(&r, TensionKind::ValueDisagreement).len(), 1);
    put_j(
        &mut r,
        "b",
        json!([{"op": "update", "id": v.to_string(), "fields": {"value": "10"}}]),
        j_typo(),
    );
    carried_equals_rebuilt(&r, "the value changes in another view");
    put_j(
        &mut r,
        "b",
        json!([{"op": "update", "id": v.to_string(), "fields": {"value": "7"}}]),
        j_typo(),
    );
    carried_equals_rebuilt(&r, "the value changes again");
    put_j(
        &mut r,
        "a",
        json!([{"op": "update", "id": y.to_string(), "body": "A job retries for 7 s."}]),
        j_typo(),
    );
    carried_equals_rebuilt(&r, "the describer follows the value");
    put_j(
        &mut r,
        "a",
        json!([{"op": "delete", "id": y.to_string()}]),
        j_typo(),
    );
    carried_equals_rebuilt(&r, "the describer is deleted");
}
