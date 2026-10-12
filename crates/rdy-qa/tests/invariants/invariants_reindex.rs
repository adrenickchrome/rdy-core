//! sg1: invariant 5: reindex reproduces the derived state and the logged moves (D-Z0EMP).
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::reindex_check::{compare, derived, logged, reindex_reproduces};
use rdy_qa::tensions::put;
use serde_json::json;

#[test]
fn sg1_reindex_reproduces_the_derived_state() {
    let (d, mut r) = project();
    let (_doc, section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let base = version_of(&r, &Scope::Canon, &c);
    put(
        &mut r,
        "a",
        json!([{"op": "update", "id": c, "base": base, "body": "A."}]),
    );
    put(
        &mut r,
        "b",
        json!([{"op": "update", "id": c, "base": base, "body": "B."}]),
    );
    put(
        &mut r,
        "b",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "B two."}]),
    );
    let first = derived(&r).unwrap();
    assert!(
        !first.tensions.is_empty(),
        "the scenario must hold a tension"
    );
    assert!(
        !first.logged_tensions.is_empty() && first.transitions > 0,
        "the moves are logged"
    );
    assert!(logged(&r).is_empty());
    r.close().unwrap();
    reindex_reproduces(d.path()).unwrap();
}

#[test]
fn sg1_a_changed_derived_state_is_a_violation() {
    // the comparison itself: two Derived values that differ give the `reindex` rule
    let (_d, mut r) = project();
    seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = derived(&r).unwrap();
    let mut b = a.clone();
    b.tensions.insert("tn-000000000000".into(), "open".into());
    let v = compare(&a, &b);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].rule, "reindex");
    assert_eq!(
        v[0].detail,
        "reindex changed tension ids: 0 only before, 1 only after"
    );
    let mut c = a.clone();
    c.transitions += 1;
    let v = compare(&a, &c);
    assert_eq!(v.len(), 1);
    assert_eq!(
        v[0].detail,
        format!(
            "reindex changed the logged moves: {} only before, {} only after",
            a.transitions, c.transitions
        )
    );
}
