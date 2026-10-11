//! cc7: restatement drift (FW-CH2, D-S46DE): an edit of text that another block restates.
use rdy_engine::tensions::{TensionKind, TensionSeverity};
use rdy_qa::fixtures::{land, project};
use rdy_qa::tensions::*;
use serde_json::json;

const SAID: &str = "Every job retries for five seconds and then reports a failure to the owner.";

/// The seed ops: x and y hold the same sentence, in two docs.
fn seed_ops() -> serde_json::Value {
    json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "jobs", "title": "Jobs", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$x", "parent": "$d", "body": SAID},
        {"op": "create", "type": "doc", "id": "$e", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$y", "parent": "$e", "body": format!("In short: {SAID}")}
    ])
}

/// Draft section with x and y holding the same sentence, landed. Returns (dir, rdy, x, y).
fn world() -> (
    tempfile::TempDir,
    rdy_engine::Rdy,
    rdy_model::BlockId,
    rdy_model::BlockId,
) {
    let (d, mut r) = project();
    let w = put(&mut r, "seed", seed_ops());
    land(&mut r, "seed").unwrap();
    (d, r, created(&w, "$x"), created(&w, "$y"))
}

/// `world` on a project whose pack extends rdy-standard with `section`.
fn world_with_pack(
    section: &str,
) -> (
    tempfile::TempDir,
    rdy_engine::Rdy,
    rdy_model::BlockId,
    rdy_model::BlockId,
) {
    let (d, r) = project();
    drop(r);
    let yaml = format!("name: drift\nversion: \"1\"\nextends: rdy-standard\n{section}");
    rdy_qa::packs::use_pack(d.path(), "drift", &yaml, "");
    let mut r = rdy_engine::Rdy::open(d.path()).expect("open");
    let w = put(&mut r, "seed", seed_ops());
    land(&mut r, "seed").unwrap();
    (d, r, created(&w, "$x"), created(&w, "$y"))
}

#[test]
fn cc7_an_edit_of_restated_text_raises_drift_with_the_other_block() {
    let (_d, mut r, x, y) = world();
    put(
        &mut r,
        "a",
        upd(
            &x,
            "Every job retries for ten seconds and then reports a failure to the owner.",
        ),
    );
    let t = one_involving(&r, TensionKind::RestatementDrift, &x);
    assert!(t.blocks().contains(&y));
    assert_eq!(t.severity, TensionSeverity::Advisory);
    assert!(t.summary.contains("changed text that"), "{}", t.summary);
}

/// guard (pre-flight): a negative test; it fails before the change only by not compiling.
#[test]
fn cc7_an_edit_outside_the_shared_text_raises_nothing() {
    let (_d, mut r, x, _y) = world();
    put(
        &mut r,
        "a",
        upd(&x, &format!("{SAID} Owners may silence it.")),
    );
    assert!(
        of_kind(&r, TensionKind::RestatementDrift).is_empty(),
        "appended text touches no shared shingle"
    );
}

#[test]
fn cc7_it_persists_in_canon_until_the_other_block_changes() {
    let (_d, mut r, x, y) = world();
    put(
        &mut r,
        "a",
        upd(
            &x,
            "Every job retries for ten seconds and then reports a failure to the owner.",
        ),
    );
    land(&mut r, "a").unwrap();
    let t = one_involving(&r, TensionKind::RestatementDrift, &x);
    assert!(has_canon(&t) && change_sides(&t).is_empty());
    put(
        &mut r,
        "b",
        upd(
            &y,
            "In short: every job retries for ten seconds and then reports a failure to the owner.",
        ),
    );
    land(&mut r, "b").unwrap();
    assert!(
        of_kind(&r, TensionKind::RestatementDrift).is_empty(),
        "y moved with x"
    );
}

/// guard (pre-flight): a negative test, as above.
#[test]
fn cc7_short_blocks_and_decisions_take_no_part() {
    let (_d, mut r) = project();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$d", "body": "Jobs retry five times."},
            {"op": "create", "type": "prose", "id": "$y", "parent": "$d", "body": "Jobs retry five times."}
        ]),
    );
    land(&mut r, "seed").unwrap();
    put(
        &mut r,
        "a",
        upd(&created(&w, "$x"), "Jobs retry six times."),
    );
    assert!(
        of_kind(&r, TensionKind::RestatementDrift).is_empty(),
        "under eight words"
    );
}

/// guard (pre-flight): a negative test, as above.
#[test]
fn cc7_the_pack_threshold_applies() {
    // a pack with coverage.drift_threshold: 100 needs identical shingle sets: "In short:" adds two shingles to y
    let (_d, mut r, x, _y) = world_with_pack("coverage:\n  drift_threshold: 100\n");
    put(
        &mut r,
        "a",
        upd(
            &x,
            "Every job retries for ten seconds and then reports a failure to the owner.",
        ),
    );
    assert!(of_kind(&r, TensionKind::RestatementDrift).is_empty());
}

/// The carried set equals a rebuild after `step`, and each view's seeded run finds what the set
/// holds for its changes.
fn carried_equals_rebuilt(r: &rdy_engine::Rdy, step: &str) {
    let st = r.read();
    let set = st.tensions().unwrap();
    let carried: Vec<_> = set.iter().map(|t| t.id).collect();
    let rebuilt: Vec<_> = st
        .tensions_rebuilt()
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(carried, rebuilt, "{step}");
    for vn in ["a", "b", "c"] {
        let inc = st.included(vn);
        let mut from_set: Vec<_> = inc
            .iter()
            .flat_map(|c| set.involving_change(c))
            .map(|t| t.id)
            .collect();
        from_set.sort();
        from_set.dedup();
        let seeded: Vec<_> = st
            .tensions_of_changes_seeded(&inc)
            .unwrap()
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(seeded, from_set, "{step} view {vn}");
    }
}

/// The drift tensions that involve `b`.
fn drifts_on(r: &rdy_engine::Rdy, b: &rdy_model::BlockId) -> usize {
    of_kind(r, TensionKind::RestatementDrift)
        .iter()
        .filter(|t| t.blocks().contains(b))
        .count()
}

#[test]
fn cc7_a_carried_drift_set_equals_a_rebuild() {
    let (_d, mut r, x, y) = world();
    let doc = r
        .read()
        .materialized(&rdy_engine::Scope::Canon)
        .unwrap()
        .get(&x)
        .and_then(|b| b.parent.clone())
        .expect("x's doc");
    put(
        &mut r,
        "a",
        upd(
            &x,
            "Every job retries for ten seconds and then reports a failure to the owner.",
        ),
    );
    carried_equals_rebuilt(&r, "x edited");
    assert_eq!(drifts_on(&r, &y), 1);
    // the restating block is edited in another view and still restates: the seed is y
    put(&mut r, "b", upd(&y, &format!("In short: {SAID} Today.")));
    carried_equals_rebuilt(&r, "y edited");
    let t = one_involving(&r, TensionKind::RestatementDrift, &y);
    assert_eq!(
        change_sides(&t).len(),
        2,
        "x's change and y's change: {t:#?}"
    );
    // a new block restates the old text
    let w = put(
        &mut r,
        "c",
        json!([
            {"op": "create", "type": "prose", "id": "$z", "parent": doc.to_string(), "body": format!("Note: {SAID}")}
        ]),
    );
    let z = created(&w, "$z");
    carried_equals_rebuilt(&r, "z created");
    assert_eq!(drifts_on(&r, &z), 1);
    // x edited again in the same view: the view's net edit still changes what y and z restate
    put(
        &mut r,
        "a",
        upd(
            &x,
            "Every job retries for twenty seconds and then reports a failure to the owner.",
        ),
    );
    carried_equals_rebuilt(&r, "x edited again");
    assert_eq!(drifts_on(&r, &z), 1, "x edited again");
    // x written back to its canon text in the view: no net edit, no drift
    put(&mut r, "a", upd(&x, SAID));
    carried_equals_rebuilt(&r, "x written back");
    assert_eq!(drifts_on(&r, &x), 0, "x written back");
    put(&mut r, "a", upd(&x, EDITED));
    carried_equals_rebuilt(&r, "x edited once more");
    put(&mut r, "b", json!([{"op": "delete", "id": y.to_string()}]));
    carried_equals_rebuilt(&r, "y deleted");
    land(&mut r, "a").unwrap();
    carried_equals_rebuilt(&r, "a landed");
}

/// The drift tensions that involve both `x` and `y`.
fn drifts_between(r: &rdy_engine::Rdy, x: &rdy_model::BlockId, y: &rdy_model::BlockId) -> usize {
    of_kind(r, TensionKind::RestatementDrift)
        .iter()
        .filter(|t| t.blocks().contains(x) && t.blocks().contains(y))
        .count()
}

const EDITED: &str = "Every job retries for ten seconds and then reports a failure to the owner.";

/// Probe A (ruling, D-S46DE): a view edits x in two writes. The second write appends text, so its
/// own base already holds the edit, but the view's net edit still changes the text y restates.
#[test]
fn cc7_a_second_edit_of_x_keeps_the_drift_in_flight() {
    let (_d, mut r, x, y) = world();
    put(&mut r, "a", upd(&x, EDITED));
    assert_eq!(drifts_between(&r, &x, &y), 1, "the first edit");
    put(
        &mut r,
        "a",
        upd(&x, &format!("{EDITED} Owners may silence it.")),
    );
    assert_eq!(drifts_between(&r, &x, &y), 1, "the second edit keeps it");
    land(&mut r, "a").unwrap();
    let t = one_involving(&r, TensionKind::RestatementDrift, &x);
    assert!(t.blocks().contains(&y));
    assert!(has_canon(&t) && change_sides(&t).is_empty(), "{t:#?}");
}

/// Probe B (ruling, D-84QSN): a later landing that edits x again keeps the canon drift; y was
/// never edited and nobody dismissed it.
#[test]
fn cc7_a_later_landing_that_edits_x_keeps_the_canon_drift() {
    let (_d, mut r, x, y) = world();
    put(&mut r, "a", upd(&x, EDITED));
    land(&mut r, "a").unwrap();
    assert_eq!(drifts_between(&r, &x, &y), 1, "landed");
    put(
        &mut r,
        "b",
        upd(&x, &format!("{EDITED} Owners may silence it.")),
    );
    land(&mut r, "b").unwrap();
    let t = one_involving(&r, TensionKind::RestatementDrift, &x);
    assert!(t.blocks().contains(&y));
    assert!(has_canon(&t) && change_sides(&t).is_empty(), "{t:#?}");
}

/// The ruling: only an edit of y (or a dismissal) resolves a canon drift, even after x moved on.
#[test]
fn cc7_editing_y_resolves_the_canon_drift() {
    let (_d, mut r, x, y) = world();
    put(&mut r, "a", upd(&x, EDITED));
    land(&mut r, "a").unwrap();
    put(
        &mut r,
        "b",
        upd(&x, &format!("{EDITED} Owners may silence it.")),
    );
    land(&mut r, "b").unwrap();
    assert_eq!(drifts_between(&r, &x, &y), 1, "x moved on; y did not");
    // y is edited and still says five: it was edited after x's change, so the drift resolves
    put(&mut r, "c", upd(&y, &format!("In short: {SAID} Today.")));
    land(&mut r, "c").unwrap();
    assert!(
        of_kind(&r, TensionKind::RestatementDrift).is_empty(),
        "y was edited"
    );
}

/// A decision is never x and never y. A prose block restates a landed decision's body and is
/// edited: the decision is no y. A proposed decision whose body another decision states is
/// edited in a second write: no drift either.
#[test]
fn cc7_a_decision_is_neither_the_edited_block_nor_the_restating_one() {
    let (_d, mut r) = project();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "decision", "id": "$q", "parent": "$s",
                "fields": {"statement": "Retry for five seconds", "status": "proposed"}, "body": SAID},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$s", "body": format!("Note: {SAID}")}
        ]),
    );
    land(&mut r, "seed").unwrap();
    let (s, q, x) = (created(&w, "$s"), created(&w, "$q"), created(&w, "$x"));
    // a decision as x: a proposed decision states the landed one's body and is edited inside it
    let w = put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "decision", "id": "$p", "parent": s.to_string(),
                "fields": {"statement": "Report retries", "status": "proposed"}, "body": SAID}]),
    );
    let p = created(&w, "$p");
    put(&mut r, "a", upd(&p, EDITED));
    assert!(
        of_kind(&r, TensionKind::RestatementDrift).is_empty(),
        "an edited decision is no x"
    );
    // a decision as y: the prose that restates the landed decision is edited
    put(&mut r, "b", upd(&x, &format!("Note: {EDITED}")));
    assert!(
        of_kind(&r, TensionKind::RestatementDrift).is_empty(),
        "a decision is no y"
    );
    land(&mut r, "b").unwrap();
    assert!(
        of_kind(&r, TensionKind::RestatementDrift).is_empty(),
        "nor in canon"
    );
    assert!(
        r.read()
            .materialized(&rdy_engine::Scope::Canon)
            .unwrap()
            .get(&q)
            .is_some()
    );
}

/// The view's net edit is judged: a view that writes x back to the text it started from has no
/// drift.
#[test]
fn cc7_a_view_that_writes_x_back_has_no_drift() {
    let (_d, mut r, x, y) = world();
    put(&mut r, "a", upd(&x, EDITED));
    assert_eq!(drifts_between(&r, &x, &y), 1);
    put(&mut r, "a", upd(&x, SAID));
    assert!(of_kind(&r, TensionKind::RestatementDrift).is_empty());
}

/// In canon, an x younger than y is judged from its first text: y landed, then x restated it in
/// a later landing, then x was edited inside the shared text (twice).
#[test]
fn cc7_a_younger_x_drifts_from_its_first_text_in_canon() {
    let (_d, mut r) = project();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$e", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$y", "parent": "$e", "body": format!("In short: {SAID}")}
        ]),
    );
    land(&mut r, "seed").unwrap();
    let (e, y) = (created(&w, "$e"), created(&w, "$y"));
    let w = put(
        &mut r,
        "later",
        json!([{"op": "create", "type": "prose", "id": "$x", "parent": e.to_string(), "body": SAID}]),
    );
    land(&mut r, "later").unwrap();
    let x = created(&w, "$x");
    assert!(of_kind(&r, TensionKind::RestatementDrift).is_empty());
    put(&mut r, "a", upd(&x, EDITED));
    land(&mut r, "a").unwrap();
    assert_eq!(drifts_between(&r, &x, &y), 1, "first edit");
    put(
        &mut r,
        "b",
        upd(&x, &format!("{EDITED} Owners may silence it.")),
    );
    land(&mut r, "b").unwrap();
    assert_eq!(drifts_between(&r, &x, &y), 1, "second edit");
}
