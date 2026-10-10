//! tprm1: premise shift: a change edits or supersedes a block another in-flight change cites.
use rdy_engine::DecisionInput;
use rdy_engine::Scope;
use rdy_engine::WriteRequest;
use rdy_engine::tensions::*;
use rdy_model::{EditorialClass, Justification};
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
    let y = created(
        &put(
            &mut r,
            "b",
            json!([{"op": "create", "type": "claim", "id": "$y",
            "parent": s.section.to_string(), "body": "Y.",
            "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
        ),
        "$y",
    );
    let d = r
        .supersede(
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
    let d = created(&d, "$decision");
    let t = one(&r, TensionKind::PremiseShift);
    assert_eq!(change_sides(&t).len(), 2);
    assert!(t.summary.contains(&s.decision.to_string()), "{}", t.summary);
    // checklist line 19: the changer's item is the superseding decision, not the target
    assert_eq!(t.blocks(), {
        let mut v = vec![d, y];
        v.sort();
        v
    });
}

/// Checklist line 17: a decision that revokes a block another view depends on shifts its
/// premise; the involved items are the revoking decision and the citer.
#[test]
fn tprm1_revoking_a_decision_another_view_depends_on_shifts_its_premise() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let y = created(
        &put(
            &mut r,
            "b",
            json!([{"op": "create", "type": "claim", "id": "$y",
            "parent": s.section.to_string(), "body": "Y.",
            "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
        ),
        "$y",
    );
    let w = put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "decision", "id": "$n", "parent": s.section.to_string(),
                "fields": {"statement": "No rule", "status": "proposed"},
                "links": {"revokes": [{"target": s.decision.to_string()}]}}]),
    );
    let n = created(&w, "$n");
    let t = one(&r, TensionKind::PremiseShift);
    assert_eq!(change_sides(&t).len(), 2, "{t:#?}");
    assert!(change_sides(&t).contains(&change_of(&w)));
    assert_eq!(t.blocks(), {
        let mut v = vec![n, y];
        v.sort();
        v
    });
}

/// Checklist line 17: a citer of a block that canon deleted shifts against canon's absent side
/// (no landing, no version).
#[test]
fn tprm1_a_citer_of_a_block_deleted_in_canon_shifts_against_canons_absent_side() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // a claim no canon block references, so its delete can land
    let x = created(
        &write(
            &mut r,
            "seed",
            json!([{"op": "create", "type": "claim", "id": "$x",
                    "parent": s.section.to_string(), "body": "X holds."}]),
        )
        .unwrap(),
        "$x",
    );
    land(&mut r, "seed").unwrap();
    let wb = put(&mut r, "b", cite(&s.section, &x));
    let (cb, y) = (change_of(&wb), created(&wb, "$y"));
    put(&mut r, "d", json!([{"op": "delete", "id": x.to_string()}]));
    let before = one(&r, TensionKind::PremiseShift);
    land(&mut r, "d").unwrap();
    let t = one(&r, TensionKind::PremiseShift);
    assert_eq!(change_sides(&t), vec![cb]);
    let canon: Vec<&Involved> = t
        .involved
        .iter()
        .filter(|i| matches!(i.side, SideRef::Canon { .. }))
        .collect();
    assert_eq!(canon.len(), 1, "{t:#?}");
    assert_eq!(canon[0].side, SideRef::Canon { landing: None });
    assert_eq!(
        (canon[0].block.clone(), canon[0].version),
        (x.clone(), None)
    );
    assert_eq!(t.blocks(), {
        let mut v = vec![x, y];
        v.sort();
        v
    });
    assert_eq!(t.vkey, before.vkey, "the landing re-keys it");
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

#[test]
fn tprm1_an_unrelated_view_that_writes_the_pin_back_hides_no_stale_premise() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let wb = put(&mut r, "b", cite(&s.section, &s.a));
    let y = created(&wb, "$y");
    put(&mut r, "a", upd(&s.a, "A changed."));
    land(&mut r, "a").unwrap();
    let before = one(&r, TensionKind::PremiseShift);
    // view e writes A back to the content b's pin holds; e shares no view with b
    put(&mut r, "e", upd(&s.a, "A holds."));
    let t = one(&r, TensionKind::PremiseShift);
    assert_eq!(t.vkey, before.vkey);
    assert!(has_canon(&t));
    assert!(t.involved.iter().any(|i| i.block == y));
}

/// A pack whose decisions carry the tracking fields `disposition` and `trigger` (trk1).
const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
      trigger: { kind: str }
    tracking: [disposition, trigger]
";

fn trk_project() -> (tempfile::TempDir, rdy_engine::Rdy) {
    let dir = tempfile::tempdir().unwrap();
    drop(rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let r = rdy_engine::Rdy::open(dir.path()).unwrap();
    (dir, r)
}

fn disposition(v: &str) -> std::collections::BTreeMap<String, serde_json::Value> {
    [("disposition".to_string(), json!(v))]
        .into_iter()
        .collect()
}

#[test]
fn tprm1_a_tracking_edit_of_a_landed_successor_shifts_no_premise() {
    let (_d, mut r) = trk_project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    let d0 = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: section.clone(),
                statement: "Ship weekly".into(),
                fields: disposition("M2d1"),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let wc = write(
        &mut r,
        "seed",
        json!([{"op": "create", "type": "claim", "id": "$c",
        "parent": section, "body": "We ship weekly.",
        "links": {"decided_by": [{"target": d0}]}}]),
    )
    .unwrap();
    let c = wc.created["$c"].clone();
    land(&mut r, "seed").unwrap();
    let d1 = r
        .supersede(
            &writer(),
            "s",
            &d0,
            None,
            DecisionInput {
                parent: section.clone(),
                statement: "Ship daily".into(),
                fields: disposition("M2d1"),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "s").unwrap();
    put(
        &mut r,
        "b",
        json!([{"op": "update", "id": c, "body": "We ship often."}]),
    );
    assert!(of_kind(&r, TensionKind::PremiseShift).is_empty());
    // a tracking-only edit of the landed successor: nothing about D0 changes
    r.write(
        &writer(),
        WriteRequest {
            view: "u".into(),
            ops: serde_json::from_value(
                json!([{"op": "update", "id": d1, "fields": {"disposition": "M2d2"}}]),
            )
            .unwrap(),
            justification: Some(Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: "re-sliced".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();
    let shifts = of_kind(&r, TensionKind::PremiseShift);
    assert!(shifts.is_empty(), "{shifts:#?}");
}

/// Final review X2 (ruling on item 26): a decision naming, in its text, the block it supersedes
/// is not a premise citation, as the graph rules for suspects.
#[test]
fn tprm1_a_decisions_reference_to_what_it_supersedes_is_not_a_citation() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    for (who, body) in [
        ("a", String::new()),
        ("b", format!("Replaces {{{{{}}}}}.", s.decision)),
    ] {
        r.supersede(
            &ag(who),
            who,
            &s.decision.to_string(),
            None,
            DecisionInput {
                parent: s.section.to_string(),
                statement: format!("{who} replaces it"),
                body,
                ..Default::default()
            },
        )
        .unwrap();
    }
    one(&r, TensionKind::Fork);
    assert!(
        of_kind(&r, TensionKind::PremiseShift).is_empty(),
        "{:#?}",
        of_kind(&r, TensionKind::PremiseShift)
    );
}

/// Checklist line 18: one shift over two citations of blocks one decision retires has one id;
/// its summary does not depend on the path that found it (a carry or a rebuild).
#[test]
fn tprm1_one_shift_over_two_citations_reads_the_same_carried_or_rebuilt() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d1: rdy_model::BlockId = rdy_model::BlockId::parse(
        &r.decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Second rule".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"],
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    // y cites the larger id first (references come before links), so its edge order is not
    // id order
    let (x1, x2) = if s.decision > d1 {
        (s.decision.clone(), d1.clone())
    } else {
        (d1.clone(), s.decision.clone())
    };
    put(
        &mut r,
        "b",
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": s.section.to_string(),
                "body": format!("Builds on {{{{{x1}}}}}."),
                "links": {"decided_by": [{"target": x2.to_string()}]}}]),
    );
    r.read().tensions().unwrap();
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "decision", "id": "$n", "parent": s.section.to_string(),
                "fields": {"statement": "One rule for both", "status": "proposed"},
                "links": {"revokes": [{"target": x1.to_string()}, {"target": x2.to_string()}]}}]),
    );
    let st = r.read();
    let carried: Vec<(String, String)> = st
        .tensions()
        .unwrap()
        .iter()
        .filter(|t| t.kind == TensionKind::PremiseShift)
        .map(|t| (tension_label(&t.id), t.summary.clone()))
        .collect();
    assert_eq!(st.tensions_carried(), Some(true));
    let rebuilt: Vec<(String, String)> = st
        .tensions_rebuilt()
        .unwrap()
        .iter()
        .filter(|t| t.kind == TensionKind::PremiseShift)
        .map(|t| (tension_label(&t.id), t.summary.clone()))
        .collect();
    assert_eq!(carried.len(), 1, "{carried:#?}");
    assert_eq!(carried, rebuilt);
    assert!(
        carried[0].1.starts_with(&x2.to_string()),
        "the smaller id: {carried:?}"
    );
}
