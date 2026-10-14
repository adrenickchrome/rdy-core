//! tovl1: area overlap: in-flight decisions in one area touching one block; open intents
//! covering blocks another view changes (read time).
use rdy_engine::tensions::*;
use rdy_engine::{DecisionInput, Rdy, Scope, WriteResult, now_ms};
use rdy_model::BlockId;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::json;

/// Doc payments/fees (area payments): section, claims X and Y; landed.
fn payments(r: &mut Rdy) -> (BlockId, BlockId, BlockId) {
    r.new_doc(
        &writer(),
        "seed",
        "payments/fees",
        Some("page"),
        Some("Fees"),
    )
    .unwrap();
    let doc = r
        .read()
        .resolve_path(&Scope::View("seed".into()), "payments/fees")
        .unwrap();
    let w = write(r, "seed", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": doc.to_string(), "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "Fees are monthly."},
        {"op": "create", "type": "claim", "id": "$y", "parent": "$s", "body": "Refunds take a week."}
    ])).unwrap();
    land(r, "seed").unwrap();
    (created(&w, "$s"), created(&w, "$x"), created(&w, "$y"))
}

fn decide(r: &mut Rdy, who: &str, parent: &BlockId, affects: &BlockId) -> WriteResult {
    r.decide(
        &ag(who),
        who,
        DecisionInput {
            parent: parent.to_string(),
            statement: format!("{who} decides"),
            affects: vec![affects.to_string()],
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn tovl1_decisions_in_one_area_touching_one_block_overlap() {
    let (_d, mut r) = qa_project();
    let (s, x, _y) = payments(&mut r);
    decide(&mut r, "a", &s, &x);
    decide(&mut r, "b", &s, &x);
    let t = one(&r, TensionKind::AreaOverlap);
    assert_eq!(change_sides(&t).len(), 2);
    assert_eq!(t.severity, TensionSeverity::Advisory);
    assert!(t.summary.contains("payments"), "{}", t.summary);
}

#[test]
fn tovl1_no_shared_block_or_no_shared_area_is_no_overlap() {
    let (_d, mut r) = qa_project();
    let (s, x, y) = payments(&mut r);
    decide(&mut r, "a", &s, &x);
    decide(&mut r, "b", &s, &y);
    assert!(of_kind(&r, TensionKind::AreaOverlap).is_empty());
    let (_d2, mut plain) = project();
    let sp = spec_doc(&mut plain);
    decide(&mut plain, "a", &sp.section, &sp.a);
    decide(&mut plain, "b", &sp.section, &sp.a);
    assert!(
        of_kind(&plain, TensionKind::AreaOverlap).is_empty(),
        "rdy-standard declares no areas"
    );
}

#[test]
fn tovl1_an_open_intent_on_a_block_another_view_changes_is_a_read_time_overlap() {
    let (_d, mut r) = qa_project();
    let (_s, x, _y) = payments(&mut r);
    let it = r
        .intent_post(&ag("c"), None, None, &[x.to_string()], "rework fees", None)
        .unwrap();
    let cb = change_of(&put(&mut r, "b", upd(&x, "Fees are weekly.")));
    let now = now_ms();
    let ts = r.read().intent_tensions(now).unwrap();
    assert_eq!(ts.len(), 1);
    let t = &ts[0];
    assert!(t.read_time);
    assert_eq!(t.kind, TensionKind::AreaOverlap);
    assert_eq!(change_sides(t), vec![cb]);
    assert!(t.sides.iter().any(|s| matches!(s, SideRef::Intent { .. })));
    assert!(
        of_kind(&r, TensionKind::AreaOverlap).is_empty(),
        "not event-derived"
    );
    assert!(
        r.read()
            .intent_tensions(it.expires_at_ms)
            .unwrap()
            .is_empty(),
        "expired at the bound"
    );
    r.intent_withdraw(&ag("c"), &it.id).unwrap();
    assert!(r.read().intent_tensions(now).unwrap().is_empty());
}

#[test]
fn tovl1_landing_one_of_the_decisions_ends_the_overlap() {
    let (_d, mut r) = qa_project();
    let (s, x, _y) = payments(&mut r);
    decide(&mut r, "a", &s, &x);
    decide(&mut r, "b", &s, &x);
    one(&r, TensionKind::AreaOverlap);
    land(&mut r, "a").unwrap();
    assert!(of_kind(&r, TensionKind::AreaOverlap).is_empty());
}

/// Checklist line 29: an intent in the view of the change that edits the block is no overlap.
#[test]
fn tovl1_an_intent_in_the_writes_own_view_is_no_overlap() {
    let (_d, mut r) = qa_project();
    let (_s, x, _y) = payments(&mut r);
    r.intent_post(
        &ag("b"),
        Some("b"),
        None,
        &[x.to_string()],
        "rework fees",
        None,
    )
    .unwrap();
    put(&mut r, "b", upd(&x, "Fees are weekly."));
    assert!(r.read().intent_tensions(now_ms()).unwrap().is_empty());
}

/// Checklist line 29: an intent on an area covers the area's blocks.
#[test]
fn tovl1_an_intent_on_an_area_covers_its_blocks() {
    let (_d, mut r) = qa_project();
    let (_s, x, _y) = payments(&mut r);
    r.intent_post(&ag("c"), None, Some("payments"), &[], "rework fees", None)
        .unwrap();
    let cb = change_of(&put(&mut r, "b", upd(&x, "Fees are weekly.")));
    let ts = r.read().intent_tensions(now_ms()).unwrap();
    assert_eq!(ts.len(), 1, "{ts:#?}");
    assert_eq!(change_sides(&ts[0]), vec![cb]);
    assert_eq!(ts[0].blocks(), vec![x]);
}

/// Checklist line 29: a rejected decision never overlaps.
#[test]
fn tovl1_a_rejected_decision_never_overlaps() {
    let (_d, mut r) = qa_project();
    let (s, x, _y) = payments(&mut r);
    decide(&mut r, "a", &s, &x);
    // a human records a rejected decision (an agent may write only proposed ones)
    write_as(
        &mut r,
        &approver(),
        "b",
        json!([{"op": "create", "type": "decision", "id": "$d", "parent": s.to_string(),
                "fields": {"statement": "b decides", "status": "rejected",
                           "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z"},
                "links": {"affects": [{"target": x.to_string()}]}}]),
    )
    .unwrap();
    assert!(of_kind(&r, TensionKind::AreaOverlap).is_empty());
}

/// Checklist line 29: two decisions whose changes share a view never overlap.
#[test]
fn tovl1_decisions_of_changes_that_share_a_view_never_overlap() {
    let (_d, mut r) = qa_project();
    let (s, x, _y) = payments(&mut r);
    decide(&mut r, "a", &s, &x);
    let cb = change_of(&decide(&mut r, "b", &s, &x));
    one(&r, TensionKind::AreaOverlap);
    r.view_include(&ag("a"), "a", &[cb.to_string()]).unwrap();
    assert!(of_kind(&r, TensionKind::AreaOverlap).is_empty());
}
