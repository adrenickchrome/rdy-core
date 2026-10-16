//! The gate on the merge (plan M2a1, merge1): a proposal whose blocks another landing changed is
//! E_PROPOSAL_MOVED at land (FW-HB1; E_STALE_BASE before M2c), a delete and a re-create under one
//! id in separate writes still validate and land.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn seed(r: &mut Rdy) -> (String, String) {
    let res = write(r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One."}
    ]))
    .unwrap();
    land(r, "seed").unwrap();
    (res.created["$s"].clone(), res.created["$c"].clone())
}

#[test]
fn merge1_a_proposal_made_before_another_landing_on_its_blocks_is_moved_at_land() {
    let (_d, mut r) = project();
    let (_s, c) = seed(&mut r);
    write(
        &mut r,
        "a",
        json!([{"op": "update", "id": c, "body": "A."}]),
    )
    .unwrap();
    write(
        &mut r,
        "b",
        json!([{"op": "update", "id": c, "body": "B."}]),
    )
    .unwrap();
    let pa = r.propose(&writer(), "a", "a", None).unwrap();
    let pb = r.propose(&writer(), "b", "b", None).unwrap();
    r.land(&approver(), &pa.id).unwrap();
    let head = r.store().canon_head().unwrap();
    let e = r.land(&approver(), &pb.id).unwrap_err();
    assert_eq!(e.code, "E_PROPOSAL_MOVED", "{e}");
    assert_eq!(r.store().canon_head().unwrap(), head);
    // gconf1: the landing rebased b and recorded the collision as a conflict; proposing never
    // refuses for it, the version marks it blocking and its landing is refused until resolved
    let again = r.propose(&writer(), "b", "again", None).unwrap();
    assert_eq!((again.conflicts.len(), again.blocking.len()), (1, 1));
    assert_eq!(
        r.land(&approver(), &again.id).unwrap_err().code,
        "E_CONFLICT_UNRESOLVED"
    );
    assert_eq!(r.store().canon_head().unwrap(), head);
    check(&r, &Scope::Canon).unwrap();
}

#[test]
fn merge1_many_moves_under_old_ids_land_whatever_the_ids() {
    let (_d, mut r) = project();
    let (s, _c) = seed(&mut r);
    let mut ids = vec![];
    for i in 0..12 {
        let res = write(&mut r, "work", json!([{"op": "create", "type": "claim", "id": "$x", "parent": s, "body": format!("Claim {i}.")}])).unwrap();
        ids.push(res.created["$x"].clone());
    }
    land(&mut r, "work").unwrap();
    for id in &ids {
        write(&mut r, "run", json!([{"op": "delete", "id": id}])).unwrap();
        write(
            &mut r,
            "run",
            json!([{"op": "create", "type": "claim", "id": id, "parent": s, "body": "Moved."}]),
        )
        .unwrap();
    }
    assert!(r.store().view_merge("run").unwrap().conflicts.is_empty());
    land(&mut r, "run").unwrap();
    for id in &ids {
        let b = r
            .read()
            .block(&Scope::Canon, &rdy_model::BlockId::parse(id).unwrap())
            .unwrap();
        assert_eq!(b.body, "Moved.");
    }
    check(&r, &Scope::Canon).unwrap();
}

/// Fix round 1: views a and b make the identical edit, then a edits again. a's reads, cold or
/// warm, and its proposal and landing see a's own order, whatever the changes' ids.
#[test]
fn merge1_a_view_that_made_the_same_edit_as_another_keeps_its_own_order() {
    for i in 0..16 {
        let (_d, mut r) = project();
        let (_s, c) = seed(&mut r);
        let same = format!("Same {i}.");
        write(
            &mut r,
            "a",
            json!([{"op": "update", "id": c, "body": same}]),
        )
        .unwrap();
        write(
            &mut r,
            "b",
            json!([{"op": "update", "id": c, "body": same}]),
        )
        .unwrap();
        write(
            &mut r,
            "a",
            json!([{"op": "update", "id": c, "body": "After."}]),
        )
        .unwrap();
        let id = rdy_model::BlockId::parse(&c).unwrap();
        let a = Scope::View("a".into());
        assert_eq!(
            r.read().block(&a, &id).unwrap().body,
            "After.",
            "run {i}: warm"
        );
        r.invalidate();
        assert_eq!(
            r.read().block(&a, &id).unwrap().body,
            "After.",
            "run {i}: cold"
        );
        land(&mut r, "a").unwrap_or_else(|e| panic!("run {i}: {e}"));
        assert_eq!(
            r.read().block(&Scope::Canon, &id).unwrap().body,
            "After.",
            "run {i}"
        );
        check(&r, &Scope::Canon).unwrap();
    }
}

/// Fix round 1: the dry run's acceptance change depends on the change in the proposing view
/// that wrote the decision, not on another view's identical copy of it, so the merge never
/// replays the acceptance before the decision exists.
#[test]
fn merge1_accepting_a_decision_another_view_holds_identically_lands() {
    for i in 0..12 {
        let (_d, mut r) = project();
        let res = write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Log", "doc_type": "decisions-log"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
        ]))
        .unwrap();
        let made = r
            .decide(
                &writer(),
                "work",
                DecisionInput {
                    parent: res.created["$s"].clone(),
                    statement: format!("Ship {i}"),
                    ..Default::default()
                },
            )
            .unwrap();
        let did = rdy_model::BlockId::parse(&made.created["$decision"]).unwrap();
        let d = r.read().block(&Scope::View("work".into()), &did).unwrap();
        raw_commit(&mut r, "copy", vec![d]);
        land(&mut r, "work").unwrap_or_else(|e| panic!("run {i}: {e}"));
        let canon = r.read().block(&Scope::Canon, &did).unwrap();
        assert_eq!(canon.field_str("status"), Some("accepted"), "run {i}");
    }
}
