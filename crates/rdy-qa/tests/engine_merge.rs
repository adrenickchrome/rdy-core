//! The gate on the merge (plan M2a1, merge1): stale proposals keep E_STALE_BASE, a delete and a
//! re-create under one id in separate writes still validate and land.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn seed(r: &mut Rdy) -> (String, String) {
    let res = write(r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One."}
    ]))
    .unwrap();
    land(r, "seed").unwrap();
    (res.created["$s"].clone(), res.created["$c"].clone())
}

#[test]
fn merge1_a_proposal_made_before_another_landing_is_e_stale_base_at_land() {
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
    let pa = r.propose(&writer(), "a", "a").unwrap();
    let pb = r.propose(&writer(), "b", "b").unwrap();
    r.land(&approver(), &pa.id).unwrap();
    let head = r.store().canon_head().unwrap();
    let e = r.land(&approver(), &pb.id).unwrap_err();
    assert_eq!(e.code, "E_STALE_BASE", "{e}");
    assert_eq!(r.store().canon_head().unwrap(), head);
    assert_eq!(
        r.propose(&writer(), "b", "again").unwrap_err().code,
        "E_STALE_BASE"
    );
    check(&mut r, &Scope::Canon).unwrap();
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
            .block(&Scope::Canon, &rdy_model::BlockId::parse(id).unwrap())
            .unwrap();
        assert_eq!(b.body, "Moved.");
    }
    check(&mut r, &Scope::Canon).unwrap();
}
