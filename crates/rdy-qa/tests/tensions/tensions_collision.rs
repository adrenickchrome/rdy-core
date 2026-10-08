//! tcol1: edit collisions: two distinct in-flight changes edit one block from one base.
use proptest::prelude::*;
use rdy_engine::Scope;
use rdy_engine::alt::AltMemberInput;
use rdy_engine::tensions::*;
use rdy_model::BlockId;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::{Value, json};

/// The base of the conflict on `id` in `view`: the version the view edited (a write on a
/// conflicted block must name it).
fn conflict_base(r: &rdy_engine::Rdy, view: &str, id: &BlockId) -> String {
    r.read()
        .conflict_on(&Scope::View(view.into()), id)
        .unwrap()
        .expect("conflicted before resolving")
        .base
        .expect("a conflict on an update has a base")
        .to_string()
}

/// One update op resolving the conflict on `id` in `view`: it names the conflict's base.
fn resolve(r: &rdy_engine::Rdy, view: &str, id: &BlockId, body: &str) -> Value {
    let base = conflict_base(r, view, id);
    json!([{"op": "update", "id": id.to_string(), "base": base, "body": body}])
}

/// One delete op resolving the conflict on `id` in `view`: it names the conflict's base.
fn resolve_by_delete(r: &rdy_engine::Rdy, view: &str, id: &BlockId) -> Value {
    let base = conflict_base(r, view, id);
    json!([{"op": "delete", "id": id.to_string(), "base": base}])
}

/// One delete op of `id`.
fn del(id: &BlockId) -> Value {
    json!([{"op": "delete", "id": id.to_string()}])
}

/// Whether `view` has a recorded conflict on `id`.
fn conflicted(r: &rdy_engine::Rdy, view: &str, id: &BlockId) -> bool {
    r.read()
        .conflict_on(&Scope::View(view.into()), id)
        .unwrap()
        .is_some()
}

#[test]
fn tcol1_two_views_editing_one_block_from_one_base_collide_once() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ca = change_of(&put(&mut r, "a", upd(&s.a, "A by a.")));
    put(&mut r, "a", upd(&s.c, "C by a."));
    let cb = change_of(&put(&mut r, "b", upd(&s.a, "A by b.")));
    let t = one(&r, TensionKind::EditCollision);
    let mut want = vec![ca, cb];
    want.sort();
    assert_eq!(change_sides(&t), want);
    assert!(!has_canon(&t));
    assert_eq!(t.severity, TensionSeverity::Blocking);
    assert!(t.involved.iter().all(|i| i.block == s.a));
    assert!(t.summary.contains("from one base"), "{}", t.summary);
}

#[test]
fn tcol1_building_on_a_change_is_not_a_collision() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A one."));
    put(&mut r, "a", upd(&s.a, "A two."));
    assert!(of_kind(&r, TensionKind::EditCollision).is_empty());
}

#[test]
fn tcol1_a_recorded_conflict_in_one_view_collides_until_a_write_resolves_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    let cb = change_of(&put(&mut r, "b", upd(&s.a, "A by b.")));
    r.view_include(&ag("a"), "a", &[cb.to_string()]).unwrap();
    assert!(
        r.read()
            .conflict_on(&Scope::View("a".into()), &s.a)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        of_kind(&r, TensionKind::EditCollision).len(),
        1,
        "the conflict and the cross-view pair are one tension"
    );
    let ops = resolve(&r, "a", &s.a, "A resolved.");
    put(&mut r, "a", ops);
    assert!(of_kind(&r, TensionKind::EditCollision).is_empty());
}

#[test]
fn tcol1_after_one_side_lands_the_other_collides_with_canon_until_resolved() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    let cb = change_of(&put(&mut r, "b", upd(&s.a, "A by b.")));
    let before = one(&r, TensionKind::EditCollision);
    land(&mut r, "a").unwrap();
    let t = one(&r, TensionKind::EditCollision);
    assert_eq!(change_sides(&t), vec![cb]);
    assert!(has_canon(&t));
    assert_eq!(t.vkey, before.vkey, "a re-key keeps the vkey");
    assert_ne!(t.id, before.id);
    assert!(
        r.read()
            .conflict_on(&Scope::View("b".into()), &s.a)
            .unwrap()
            .is_some(),
        "the rebase recorded the conflict: the tension's continuation"
    );
    let ops = resolve(&r, "b", &s.a, "A merged.");
    put(&mut r, "b", ops);
    assert!(
        of_kind(&r, TensionKind::EditCollision).is_empty(),
        "the resolution depends on the landed side: it is not a new collision with canon"
    );
}

#[test]
fn tcol1_members_of_one_alternative_set_do_not_collide() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    one(&r, TensionKind::EditCollision);
    let m = |label: &str, view: &str| AltMemberInput {
        label: label.into(),
        view: view.into(),
    };
    r.alt_create(&ag("o"), "A's wording", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    assert!(
        of_kind(&r, TensionKind::EditCollision).is_empty(),
        "the set's own rules govern its members"
    );
    // a view outside the set collides with each member
    let cc = change_of(&put(&mut r, "c", upd(&s.a, "A by c.")));
    let ts = of_kind(&r, TensionKind::EditCollision);
    assert_eq!(ts.len(), 2, "{ts:#?}");
    assert!(
        ts.iter()
            .all(|t| change_sides(t).len() == 2 && change_sides(t).contains(&cc))
    );
}

#[test]
fn tcol1_a_member_that_includes_an_outsiders_change_still_collides_with_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let m = |label: &str, view: &str| AltMemberInput {
        label: label.into(),
        view: view.into(),
    };
    r.alt_create(&ag("o"), "A's wording", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    let cc = change_of(&put(&mut r, "c", upd(&s.a, "A by c.")));
    assert_eq!(of_kind(&r, TensionKind::EditCollision).len(), 2);
    // member b takes the outsider's change: c's change is now in a member view and in c
    r.view_include(&ag("b"), "b", &[cc.to_string()]).unwrap();
    assert!(conflicted(&r, "b", &s.a));
    let ts = of_kind(&r, TensionKind::EditCollision);
    assert_eq!(
        ts.len(),
        2,
        "the outsider still collides with each member: {ts:#?}"
    );
    assert!(
        ts.iter()
            .all(|t| change_sides(t).len() == 2 && change_sides(t).contains(&cc))
    );
}

#[test]
fn tcol1_a_view_that_writes_a_block_twice_still_collides() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ca = change_of(&put(&mut r, "a", upd(&s.a, "A by a.")));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let cb = change_of(&put(&mut r, "b", upd(&s.a, "A by b, again.")));
    let before = one(&r, TensionKind::EditCollision);
    let mut want = vec![ca, cb];
    want.sort();
    assert_eq!(
        change_sides(&before),
        want,
        "b's second write edits A from b's first base"
    );
    land(&mut r, "a").unwrap();
    let t = one(&r, TensionKind::EditCollision);
    assert_eq!(change_sides(&t), vec![cb]);
    assert!(has_canon(&t));
    assert_eq!(t.vkey, before.vkey, "a re-key keeps the vkey");
    assert!(
        conflicted(&r, "b", &s.a),
        "the recorded conflict is the tension's continuation"
    );
    let ops = resolve(&r, "b", &s.a, "A merged.");
    put(&mut r, "b", ops);
    assert!(!conflicted(&r, "b", &s.a));
    assert!(of_kind(&r, TensionKind::EditCollision).is_empty());
}

#[test]
fn tcol1_an_update_against_a_canon_delete_clears_when_resolved_by_an_update() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", del(&s.c));
    let cb = change_of(&put(&mut r, "b", upd(&s.c, "C by b.")));
    one(&r, TensionKind::EditCollision);
    land(&mut r, "a").unwrap();
    let t = one(&r, TensionKind::EditCollision);
    assert_eq!(change_sides(&t), vec![cb]);
    assert!(has_canon(&t));
    assert!(conflicted(&r, "b", &s.c));
    let ops = resolve(&r, "b", &s.c, "C kept by b.");
    put(&mut r, "b", ops);
    assert!(!conflicted(&r, "b", &s.c));
    assert!(
        of_kind(&r, TensionKind::EditCollision).is_empty(),
        "the resolution is not a new collision with canon's delete"
    );
}

#[test]
fn tcol1_an_update_against_a_canon_delete_clears_when_resolved_by_a_delete() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", del(&s.c));
    put(&mut r, "b", upd(&s.c, "C by b."));
    land(&mut r, "a").unwrap();
    one(&r, TensionKind::EditCollision);
    let ops = resolve_by_delete(&r, "b", &s.c);
    put(&mut r, "b", ops);
    assert!(!conflicted(&r, "b", &s.c));
    assert!(
        of_kind(&r, TensionKind::EditCollision).is_empty(),
        "taking canon's side is not a collision"
    );
}

#[test]
fn tcol1_an_identical_edit_is_not_a_collision_before_or_after_a_landing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A same."));
    put(&mut r, "b", upd(&s.a, "A same."));
    assert!(
        of_kind(&r, TensionKind::EditCollision).is_empty(),
        "the merge reads one outcome as clean"
    );
    land(&mut r, "a").unwrap();
    assert!(!conflicted(&r, "b", &s.a));
    assert!(of_kind(&r, TensionKind::EditCollision).is_empty());
}

#[test]
fn tcol1_a_chain_through_canons_version_is_not_a_collision_after_a_landing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A same."));
    put(&mut r, "a", upd(&s.a, "A later."));
    put(&mut r, "b", upd(&s.a, "A same."));
    one(&r, TensionKind::EditCollision);
    land(&mut r, "b").unwrap();
    assert!(
        !conflicted(&r, "a", &s.a),
        "the merge reads a's chain as built on canon's version"
    );
    assert!(
        of_kind(&r, TensionKind::EditCollision).is_empty(),
        "a chain that passes through canon's version is not stale"
    );
}

#[test]
fn tcol1_a_chain_whose_middle_write_meets_canons_version_still_collides_after_a_landing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A first."));
    put(&mut r, "a", upd(&s.a, "A same."));
    put(&mut r, "a", upd(&s.a, "A later."));
    put(&mut r, "b", upd(&s.a, "A same."));
    land(&mut r, "b").unwrap();
    assert!(
        conflicted(&r, "a", &s.a),
        "the merge reads a's first write as against canon's version"
    );
    let t = one(&r, TensionKind::EditCollision);
    assert!(has_canon(&t), "the recorded conflict is the tension");
}

#[test]
fn tcol1_a_chain_that_ends_at_canons_version_still_collides_after_a_landing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A first."));
    put(&mut r, "a", upd(&s.a, "A same."));
    put(&mut r, "b", upd(&s.a, "A same."));
    land(&mut r, "b").unwrap();
    assert!(
        conflicted(&r, "a", &s.a),
        "the merge reads a's first write as against canon's version"
    );
    let t = one(&r, TensionKind::EditCollision);
    assert!(has_canon(&t), "the recorded conflict is the tension");
}

#[test]
fn tcol1_a_double_delete_is_not_a_collision_before_or_after_a_landing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", del(&s.c));
    put(&mut r, "b", del(&s.c));
    assert!(of_kind(&r, TensionKind::EditCollision).is_empty());
    land(&mut r, "a").unwrap();
    assert!(!conflicted(&r, "b", &s.c));
    assert!(of_kind(&r, TensionKind::EditCollision).is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(16), ..ProptestConfig::default() })]
    #[test]
    fn tcol1_fork_plus_include_never_yields_a_self_collision(
        steps in proptest::collection::vec((0u8..4, 0u8..3, 0u8..3), 1..10)
    ) {
        let (_d, mut r) = project();
        let s = spec_doc(&mut r);
        let names = ["a", "b", "c"];
        for (i, (kind, x, y)) in steps.into_iter().enumerate() {
            let (vx, vy) = (names[x as usize], names[y as usize]);
            match kind {
                0 => { let _ = try_put(&mut r, vx, upd(&s.a, &format!("A {i}"))); }
                1 => { let _ = try_put(&mut r, vx, upd(&s.c, &format!("C {i}"))); }
                2 => {
                    let inc = r.read().included(vx);
                    if let Some(c) = inc.last() {
                        let _ = r.view_include(&ag(vy), vy, &[c.to_string()]);
                    }
                }
                _ => {
                    let name = format!("f{i}");
                    if r.read().has_view(vx) {
                        let _ = r.view_fork(&ag(&name), vx, &name);
                    }
                }
            }
            for t in of_kind(&r, TensionKind::EditCollision) {
                prop_assert!(t.involved.len() >= 2, "{:?}", t);
                prop_assert!(
                    change_sides(&t).len() + usize::from(has_canon(&t)) >= 2,
                    "a change never collides with itself: {:?}", t
                );
            }
        }
    }
}
