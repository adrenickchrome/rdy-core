//! tcol1: edit collisions: two distinct in-flight changes edit one block from one base.
use proptest::prelude::*;
use rdy_engine::Scope;
use rdy_engine::alt::AltMemberInput;
use rdy_engine::tensions::*;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::{Value, json};

/// One update op resolving the conflict on `id` in `view`: it names the conflict's base, the
/// version the view edited (a write on a conflicted block must name it).
fn resolve(r: &rdy_engine::Rdy, view: &str, id: &rdy_model::BlockId, body: &str) -> Value {
    let base = r
        .read()
        .conflict_on(&Scope::View(view.into()), id)
        .unwrap()
        .expect("conflicted before resolving")
        .base
        .expect("a conflict on an update has a base");
    json!([{"op": "update", "id": id.to_string(), "base": base.to_string(), "body": body}])
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
