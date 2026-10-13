//! mfb2: FW-LP6 (D-SCJGF). A decision that refines or part-supersedes another reaches what the
//! other governs: a `dependent` entry with follow-up `confirm` per governed block, and a `reaches`
//! section in the new decision's briefing.
use rdy_engine::brief::{FollowUp, Reach};
use rdy_engine::consequence::*;
use rdy_engine::*;
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use serde_json::json;

fn w() -> Scope {
    Scope::View("w".into())
}

fn after(r: &Rdy, change: &str) -> Consequences {
    r.read()
        .consequences(&w(), &change.parse::<Hash>().unwrap(), None)
        .unwrap()
}

fn entries<'a>(c: &'a Consequences, block: &BlockId) -> Vec<&'a Consequence> {
    c.items
        .iter()
        .filter(|x| x.block == block.to_string())
        .collect()
}

fn refine(r: &mut Rdy, s: &Spec, target: &BlockId) -> WriteResult {
    r.decide(
        &writer(),
        "w",
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Keep A, in short form".into(),
            refines: vec![target.to_string()],
            ..Default::default()
        },
    )
    .unwrap()
}

/// A decision made in view `d` and landed that `affects` `on`.
fn landed_affecting(r: &mut Rdy, s: &Spec, on: &BlockId) -> BlockId {
    let d = r
        .decide(
            &writer(),
            "d",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "C stays short".into(),
                affects: vec![on.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let d = BlockId::parse(&d.created["$decision"]).unwrap();
    land(r, "d").unwrap();
    d
}

#[test]
fn mfb2_a_refining_decision_asks_to_confirm_what_the_refined_one_governs() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let res = refine(&mut r, &s, &s.decision);
    let c = after(&r, &res.change);
    let a = entries(&c, &s.a);
    assert_eq!(a.len(), 1, "{c:?}");
    let e = a[0];
    assert_eq!(e.kind, ConsequenceKind::Dependent);
    assert_eq!(e.follow_up, FollowUp::Confirm);
    assert_eq!(e.because.edge, "refines");
    assert_eq!(e.because.changed, s.decision.to_string());
    assert_eq!(e.fix.as_deref(), Some(LINEAGE_FIX));
    assert!(!e.already_written);
    assert_eq!(
        e.id,
        consequence_id(&res.change.parse::<Hash>().unwrap(), "dependent", &s.a)
    );
}

#[test]
fn mfb2_the_refined_decisions_affects_are_reached() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d2 = landed_affecting(&mut r, &s, &s.c);
    let res = refine(&mut r, &s, &d2);
    let c = after(&r, &res.change);
    let e = entries(&c, &s.c);
    assert_eq!(e.len(), 1, "{c:?}");
    assert_eq!(e[0].follow_up, FollowUp::Confirm);
    assert_eq!(e[0].because.changed, d2.to_string());
}

#[test]
fn mfb2_a_part_supersede_reaches_the_affects_of_its_target() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d2 = landed_affecting(&mut r, &s, &s.c);
    let res = r
        .supersede(
            &writer(),
            "w",
            &d2.to_string(),
            Some("short".into()),
            DecisionInput {
                parent: s.section.to_string(),
                statement: "C stays short, mostly".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let c = after(&r, &res.change);
    let e = entries(&c, &s.c);
    assert_eq!(e.len(), 1, "{c:?}");
    assert_eq!(e[0].because.edge, "supersedes");
    assert_eq!(e[0].follow_up, FollowUp::Confirm);
}

/// guard: passes before the fix (it pins Q4: the reach pass must not add a second entry).
#[test]
fn mfb2_a_block_already_listed_keeps_its_entry() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // a part supersede of D lists A already, through decided_by (dependent, update); the reach
    // pass adds no second entry for A
    let res = r
        .supersede(
            &writer(),
            "w",
            &s.decision.to_string(),
            Some("form".into()),
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Keep A, in any form".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let c = after(&r, &res.change);
    let a = entries(&c, &s.a);
    assert_eq!(a.len(), 1, "{c:?}");
    assert_eq!(a[0].because.edge, "decided_by");
    assert_eq!(a[0].follow_up, FollowUp::Update);
}

#[test]
fn mfb2_a_block_the_view_wrote_is_already_written() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A, edited first."}]),
    )
    .unwrap();
    let res = refine(&mut r, &s, &s.decision);
    let c = after(&r, &res.change);
    assert!(entries(&c, &s.a)[0].already_written);
}

/// guard: passes before the fix (it pins Q5/Q6: a full supersede does not reach).
#[test]
fn mfb2_a_full_supersede_reaches_nothing_new() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d2 = landed_affecting(&mut r, &s, &s.c);
    let res = r
        .supersede(
            &writer(),
            "w",
            &d2.to_string(),
            None,
            DecisionInput {
                parent: s.section.to_string(),
                statement: "C may grow".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(entries(&after(&r, &res.change), &s.c).is_empty());
}

#[test]
fn mfb2_the_refining_decisions_briefing_lists_what_it_reaches() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let res = refine(&mut r, &s, &s.decision);
    let new = BlockId::parse(&res.created["$decision"]).unwrap();
    let b = brief_of(&r, &w(), &[&new]);
    assert_eq!(b.reaches.len(), 1, "{:?}", b.reaches);
    let reach = &b.reaches[0];
    assert_eq!(reach.decision, new.to_string());
    assert_eq!(reach.via, "refines");
    assert_eq!(reach.target, s.decision.to_string());
    assert!(reach.blocks.contains(&s.a.to_string()), "{reach:?}");
    assert_eq!(b.reaches_omitted, 0);
    let v = serde_json::to_value(&b).unwrap();
    assert!(v["reaches"].is_array(), "{v}");
}

/// A decision made in view `view` from `d` and landed there.
fn landed(r: &mut Rdy, view: &str, d: DecisionInput) -> BlockId {
    let res = r.decide(&writer(), view, d).unwrap();
    let id = BlockId::parse(&res.created["$decision"]).unwrap();
    land(r, view).unwrap();
    id
}

/// P2: a governed decision that is accepted keeps what the walk gives it (reaffirm), never a
/// confirm that a repin of a frozen decision refuses.
#[test]
fn mfb2_a_governed_accepted_decision_gets_reaffirm() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d2 = landed_affecting(&mut r, &s, &s.decision);
    let res = refine(&mut r, &s, &d2);
    let c = after(&r, &res.change);
    let e = entries(&c, &s.decision);
    assert_eq!(e.len(), 1, "{c:?}");
    assert_eq!(e[0].kind, ConsequenceKind::Dependent);
    assert_eq!(e[0].follow_up, FollowUp::Reaffirm);
    assert_eq!(e[0].fix.as_deref(), Some(REAFFIRM_FIX));
    assert_eq!(e[0].because.edge, "refines");
    assert_eq!(e[0].because.changed, d2.to_string());
}

/// P5: a governed decision that asserts nothing live (fully superseded) gets no entry, and the
/// briefing does not list it.
#[test]
fn mfb2_a_governed_superseded_decision_gets_no_entry() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d2 = landed_affecting(&mut r, &s, &s.decision);
    r.supersede(
        &writer(),
        "x",
        &s.decision.to_string(),
        None,
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Keep A, firmly".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    let res = refine(&mut r, &s, &d2);
    let c = after(&r, &res.change);
    assert!(entries(&c, &s.decision).is_empty(), "{c:?}");
    let new = BlockId::parse(&res.created["$decision"]).unwrap();
    let b = brief_of(&r, &w(), &[&new]);
    assert!(
        b.reaches
            .iter()
            .all(|x| !x.blocks.contains(&s.decision.to_string())),
        "{:?}",
        b.reaches
    );
}

/// A governed decision that is only proposed keeps what the walk gives it: update, no fix.
#[test]
fn mfb2_a_governed_proposed_decision_gets_update() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let p = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "C is short".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let p = BlockId::parse(&p.created["$decision"]).unwrap();
    let d2 = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "C stays short".into(),
                affects: vec![p.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let d2 = BlockId::parse(&d2.created["$decision"]).unwrap();
    let res = refine(&mut r, &s, &d2);
    let c = after(&r, &res.change);
    let e = entries(&c, &p);
    assert_eq!(e.len(), 1, "{c:?}");
    assert_eq!(e[0].kind, ConsequenceKind::Dependent);
    assert_eq!(e[0].follow_up, FollowUp::Update);
    assert_eq!(e[0].fix, None);
    assert_eq!(e[0].because.edge, "refines");
}

/// P3: a full supersede copies the old decision's refines links (sup1), but it does not reach
/// through them (Q5, Q6): no entry for what the refined decision governs, and no reaches in the
/// new decision's briefing.
#[test]
fn mfb2_a_full_supersede_of_a_refining_decision_reaches_nothing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d2 = landed(
        &mut r,
        "d",
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Keep A, in short form".into(),
            refines: vec![s.decision.to_string()],
            ..Default::default()
        },
    );
    let res = r
        .supersede(
            &writer(),
            "w",
            &d2.to_string(),
            None,
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Keep A, in long form".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let c = after(&r, &res.change);
    assert!(entries(&c, &s.a).is_empty(), "{c:?}");
    let new = BlockId::parse(&res.created["$decision"]).unwrap();
    let b = brief_of(&r, &w(), &[&new]);
    assert!(b.reaches.is_empty(), "{:?}", b.reaches);
    assert_eq!(b.reaches_omitted, 0);
}

/// Final review 2, finding 10: a full supersede skips only the refines links it copied from the
/// decision it supersedes. Refines given to rdy_supersede reach as any refine does, a target the
/// old decision also refines among them.
#[test]
fn mfb2_an_explicit_refines_on_a_full_supersede_reaches() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d3 = landed_affecting(&mut r, &s, &s.c);
    let d2 = landed(
        &mut r,
        "e",
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Keep A, in short form".into(),
            refines: vec![s.decision.to_string()],
            ..Default::default()
        },
    );
    let res = r
        .supersede(
            &writer(),
            "w",
            &d2.to_string(),
            None,
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Keep A, in long form".into(),
                refines: vec![s.decision.to_string(), d3.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let c = after(&r, &res.change);
    for (block, target) in [(&s.a, &s.decision), (&s.c, &d3)] {
        let e = entries(&c, block);
        assert_eq!(e.len(), 1, "{block}: {c:?}");
        assert_eq!(e[0].kind, ConsequenceKind::Dependent);
        assert_eq!(e[0].because.edge, "refines");
        assert_eq!(e[0].because.changed, target.to_string());
    }
    let new = BlockId::parse(&res.created["$decision"]).unwrap();
    let b = brief_of(&r, &w(), &[&new]);
    // sorted by target id (then decision, then via), whatever the graph's iteration order
    let mut want = vec![
        (s.decision.to_string(), s.a.to_string()),
        (d3.to_string(), s.c.to_string()),
    ];
    want.sort();
    let want: Vec<Reach> = want
        .into_iter()
        .map(|(target, block)| Reach {
            decision: new.to_string(),
            via: "refines".into(),
            target,
            blocks: vec![block],
        })
        .collect();
    assert_eq!(b.reaches, want);
}

/// P8: a block deleted in the view is not reached, in the consequences or in the briefing.
#[test]
fn mfb2_a_block_deleted_in_the_view_is_not_reached() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d2 = landed_affecting(&mut r, &s, &s.c);
    write(
        &mut r,
        "w",
        json!([{"op": "delete", "id": s.c.to_string()}]),
    )
    .unwrap();
    let res = refine(&mut r, &s, &d2);
    let c = after(&r, &res.change);
    assert!(entries(&c, &s.c).is_empty(), "{c:?}");
    let new = BlockId::parse(&res.created["$decision"]).unwrap();
    let b = brief_of(&r, &w(), &[&new]);
    assert!(
        b.reaches
            .iter()
            .all(|x| !x.blocks.contains(&s.c.to_string())),
        "{:?}",
        b.reaches
    );
    assert_eq!(b.reaches_omitted, 0);
}

/// Agents learn through tools only: the edit-loop doc in `rdy_schema` says what a refine reaches.
#[test]
fn mfb2_the_edit_loop_doc_says_what_a_refine_reaches() {
    let doc = rdy_mcp::tools::EDIT_LOOP_DOC;
    assert!(
        doc.contains(
            "A write that records a decision with refines, or a supersedes with a part, also lists each block the refined or partly superseded decision governs (decided_by, implements, its affects): kind dependent, follow_up confirm (a governed decision gets reaffirm, or update while proposed, and no entry when it asserts nothing live), because.edge refines or supersedes. A full supersede reaches only through the refines passed to rdy_supersede, not through the ones it copies (a refines list equal to the old decision's counts as copied)."
        ),
        "{doc}"
    );
    assert!(
        doc.contains("A briefing of the new decision lists them in reaches: {decision, via, target, blocks}; reaches_omitted counts the blocks past 50."),
        "{doc}"
    );
}

/// Final review M5: a target that asserts nothing live (rejected, superseded or revoked) reaches
/// nothing, in the consequences or in the briefing.
#[test]
fn mfb2_a_target_that_asserts_nothing_live_reaches_nothing() {
    for status in ["rejected", "superseded", "revoked"] {
        let (_d, mut r) = project();
        let s = spec_doc(&mut r);
        let target = if status == "rejected" {
            // an approver may write a decision with a non-proposed status
            let res = rdy_qa::golden::write_as(
                &approver(),
                &mut r,
                "d",
                json!([{"op": "create", "type": "decision", "id": "$x", "parent": s.section.to_string(),
                    "fields": {"statement": "C stays short", "status": "rejected",
                               "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z",
                               "options": ["yes", "no"]},
                    "links": {"affects": [{"target": s.c.to_string()}]}}]),
            )
            .unwrap();
            let x = BlockId::parse(&res.created["$x"]).unwrap();
            land(&mut r, "d").unwrap();
            x
        } else {
            let d2 = landed_affecting(&mut r, &s, &s.c);
            if status == "superseded" {
                r.supersede(
                    &writer(),
                    "x",
                    &d2.to_string(),
                    None,
                    DecisionInput {
                        parent: s.section.to_string(),
                        statement: "C may grow".into(),
                        ..Default::default()
                    },
                )
                .unwrap();
            } else {
                r.revoke(
                    &writer(),
                    "x",
                    &d2.to_string(),
                    "not needed",
                    &s.section.to_string(),
                    None,
                )
                .unwrap();
            }
            land(&mut r, "x").unwrap();
            d2
        };
        let res = refine(&mut r, &s, &target);
        let c = after(&r, &res.change);
        // FW-LP6 reach entries only: the new decision's own kept suspect (df4 ruling 2: a
        // `refines` to a superseded or revoked decision makes it suspect) is not a reach
        assert!(
            c.items.iter().all(|e| e.kind != ConsequenceKind::Dependent
                || e.because.changed != target.to_string()
                || e.because.edge != "refines"),
            "{status}: {c:?}"
        );
        assert!(entries(&c, &s.c).is_empty(), "{status}: {c:?}");
        let new = BlockId::parse(&res.created["$decision"]).unwrap();
        let b = brief_of(&r, &w(), &[&new]);
        assert!(b.reaches.is_empty(), "{status}: {:?}", b.reaches);
        assert_eq!(b.reaches_omitted, 0, "{status}");
    }
}

/// Final review M5 (guard): a governed block that the reaching change itself writes gets no
/// reach entry.
#[test]
fn mfb2_a_governed_block_the_change_writes_gets_no_reach_entry() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let res = rdy_qa::golden::write_as(
        &approver(),
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "decision", "id": "$x", "parent": s.section.to_string(),
             "fields": {"statement": "Keep A, in short form", "status": "proposed",
                        "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z",
                        "options": ["yes", "no"]},
             "links": {"refines": [{"target": s.decision.to_string()}]}},
            {"op": "update", "id": s.a.to_string(), "body": "A holds, in short form."}
        ]),
    )
    .unwrap();
    let c = after(&r, &res.change);
    assert!(entries(&c, &s.a).is_empty(), "{c:?}");
}

/// `n` claims under the spec's section, each `decided_by` a decision that is landed.
fn landed_governing(r: &mut Rdy, s: &Spec, n: usize) -> BlockId {
    let d = landed(
        r,
        "g",
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Every rule is short".into(),
            ..Default::default()
        },
    );
    let ops: Vec<_> = (0..n)
        .map(|i| {
            json!({"op": "create", "type": "claim", "parent": s.section.to_string(),
                   "body": format!("Rule {i} is short."),
                   "links": {"decided_by": [{"target": d.to_string()}]}})
        })
        .collect();
    write(r, "g", json!(ops)).unwrap();
    land(r, "g").unwrap();
    d
}

/// Final review M5: a briefing lists at most `REACH_CAP` blocks in all and counts the rest in
/// `reaches_omitted`; a link past the cap is counted, never listed with no blocks.
#[test]
fn mfb2_reaches_stop_at_the_cap_and_count_the_rest() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let many = landed_governing(&mut r, &s, rdy_engine::brief::REACH_CAP + 5);
    let res = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Every rule is short, and A too".into(),
                refines: vec![many.to_string(), s.decision.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let new = BlockId::parse(&res.created["$decision"]).unwrap();
    let b = brief_of(&r, &w(), &[&new]);
    let listed: usize = b.reaches.iter().map(|x| x.blocks.len()).sum();
    assert_eq!(listed, rdy_engine::brief::REACH_CAP, "{:?}", b.reaches);
    // 55 rules and A: 56 reached, 50 listed
    assert_eq!(b.reaches_omitted, 6);
    assert!(
        b.reaches.iter().all(|x| !x.blocks.is_empty()),
        "{:?}",
        b.reaches
    );
}

/// Final review M5: over several reaching links, the entries come by target, then by block.
#[test]
fn mfb2_reach_entries_come_by_target_then_block() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d2 = landed(
        &mut r,
        "d",
        DecisionInput {
            parent: s.section.to_string(),
            statement: "B and C stay short".into(),
            affects: vec![s.c.to_string(), s.b.to_string()],
            ..Default::default()
        },
    );
    // the links are given in the reverse of the order the entries come in
    let mut targets = vec![s.decision.to_string(), d2.to_string()];
    targets.sort();
    targets.reverse();
    let res = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Keep A, B and C short".into(),
                refines: targets,
                ..Default::default()
            },
        )
        .unwrap();
    let c = after(&r, &res.change);
    let got: Vec<(String, String)> = c
        .items
        .iter()
        .filter(|e| e.because.edge == "refines")
        .map(|e| (e.because.changed.clone(), e.block.clone()))
        .collect();
    let mut want = vec![
        (s.decision.to_string(), s.a.to_string()),
        (d2.to_string(), s.b.to_string()),
        (d2.to_string(), s.c.to_string()),
    ];
    want.sort();
    assert_eq!(got, want, "{c:?}");
}
