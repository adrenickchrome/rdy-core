//! tprm1: premise shift: a change edits or supersedes a block another in-flight change cites.
use rdy_engine::DecisionInput;
use rdy_engine::Scope;
use rdy_engine::tensions::*;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::json;
use std::collections::BTreeSet;

#[test]
fn tprm1_an_edit_to_a_block_another_view_cites_shifts_its_premise() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let wb = put(&mut r, "b", cite(&s.section, &s.a));
    let (cb, y) = (change_of(&wb), created(&wb, "$y"));
    let ca = change_of(&put(&mut r, "a", upd(&s.a, "A changed.")));
    let t = one(&r, TensionKind::PremiseShift);
    let mut want = vec![ca, cb];
    want.sort();
    assert_eq!(change_sides(&t), want);
    let blocks: BTreeSet<_> = t.involved.iter().map(|i| i.block.clone()).collect();
    assert_eq!(blocks, BTreeSet::from([s.a.clone(), y]));
    assert_eq!(t.severity, TensionSeverity::Blocking);
    assert!(
        !t.involved.iter().any(|i| i.block == s.b),
        "canon's B is a suspect, not a tension"
    );
}

#[test]
fn tprm1_after_the_edit_lands_the_shift_rekeys_to_canon_until_the_citer_repins() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let wb = put(&mut r, "b", cite(&s.section, &s.a));
    let (cb, y) = (change_of(&wb), created(&wb, "$y"));
    put(&mut r, "a", upd(&s.a, "A changed."));
    let before = one(&r, TensionKind::PremiseShift);
    land(&mut r, "a").unwrap();
    let t = one(&r, TensionKind::PremiseShift);
    assert_eq!(change_sides(&t), vec![cb]);
    assert!(has_canon(&t));
    assert_eq!(t.vkey, before.vkey);
    put(&mut r, "b", json!([{"op": "repin", "id": y.to_string()}]));
    assert!(of_kind(&r, TensionKind::PremiseShift).is_empty());
}

#[test]
fn tprm1_superseding_a_decision_another_view_depends_on_shifts_its_premise() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(
        &mut r,
        "b",
        json!([{"op": "create", "type": "claim", "id": "$y",
        "parent": s.section.to_string(), "body": "Y.",
        "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
    );
    r.supersede(
        &ag("a"),
        "a",
        &s.decision.to_string(),
        None,
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Keep A, differently".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let t = one(&r, TensionKind::PremiseShift);
    assert_eq!(change_sides(&t).len(), 2);
    assert!(t.summary.contains(&s.decision.to_string()), "{}", t.summary);
}

#[test]
fn tprm1_deleting_a_cited_block_shifts_its_premise() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "b", cite(&s.section, &s.c));
    put(
        &mut r,
        "a",
        json!([{"op": "delete", "id": s.c.to_string()}]),
    );
    one(&r, TensionKind::PremiseShift);
}

#[test]
fn tprm1_a_citer_in_the_same_view_is_not_a_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", cite(&s.section, &s.a));
    put(&mut r, "a", upd(&s.a, "A changed."));
    assert!(of_kind(&r, TensionKind::PremiseShift).is_empty());
}

#[test]
fn tprm1_a_stale_pin_in_canon_is_a_suspect_not_a_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A changed."));
    land(&mut r, "a").unwrap();
    // canon's B still pins A's old content: a suspect, and no premise shift in any scope
    assert!(of_kind(&r, TensionKind::PremiseShift).is_empty());
    let canon = r.read().canon_tensions_rebuilt().unwrap();
    assert!(
        canon.iter().all(|t| t.kind != TensionKind::PremiseShift),
        "{canon:#?}"
    );
    let sus = r.read().suspects(&Scope::Canon).unwrap();
    assert!(
        sus.iter().any(|x| x.block == s.b && x.target == s.a),
        "{sus:#?}"
    );
}
