//! csq1: consequences from the published state after a write, each with a stable id.
use rdy_engine::brief::FollowUp;
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

fn entry<'a>(c: &'a Consequences, block: &BlockId) -> &'a Consequence {
    c.items
        .iter()
        .find(|x| x.block == block.to_string())
        .unwrap_or_else(|| panic!("no entry for {block}: {c:?}"))
}

fn has(c: &Consequences, block: &BlockId) -> bool {
    c.items.iter().any(|x| x.block == block.to_string())
}

fn update_a(r: &mut Rdy, s: &Spec) -> WriteResult {
    write(
        r,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A changed."}]),
    )
    .unwrap()
}

/// An accepted decision (made in view `d` and landed) with `depends_on: deps`.
fn accepted_depending_on(r: &mut Rdy, s: &Spec, deps: &[&BlockId]) -> BlockId {
    let d = r
        .decide(
            &writer(),
            "d",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Build on it".into(),
                depends_on: deps.iter().map(|b| b.to_string()).collect(),
                ..Default::default()
            },
        )
        .unwrap();
    let d = BlockId::parse(&d.created["$decision"]).unwrap();
    land(r, "d").unwrap();
    d
}

/// Supersedes `old` in view `w` (the superseding decision is only proposed there).
fn supersede_in_w(r: &mut Rdy, s: &Spec, old: &BlockId) -> WriteResult {
    r.supersede(
        &writer(),
        "w",
        &old.to_string(),
        None,
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Keep A, firmly".into(),
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn csq1_updating_a_referenced_block_asks_its_normative_referrer_to_confirm() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    let b = entry(&c, &s.b);
    // D-CFC6V: B states no literal the change removed
    assert_eq!(
        (b.kind, b.follow_up),
        (ConsequenceKind::Suspect, FollowUp::Confirm)
    );
    assert_eq!(
        b.because,
        Because {
            edge: "references".into(),
            changed: s.a.to_string(),
            edges: Some(vec!["references".into()]),
            values: Some(vec![]),
            values_omitted: 0,
        }
    );
    assert!(
        c.items.iter().all(|x| x.block != s.a.to_string()),
        "the changed block itself is not listed"
    );
    assert!(c.items.iter().all(|x| x.block != s.c.to_string()));
}

#[test]
fn csq1_a_premise_link_is_update_and_an_accepted_decision_is_reaffirm_with_the_fix() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d2 = r
        .decide(
            &writer(),
            "d",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Build on A".into(),
                depends_on: vec![s.a.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let d2 = BlockId::parse(&d2.created["$decision"]).unwrap();
    write(
        &mut r,
        "d",
        json!([{"op": "create", "type": "claim", "id": "$p", "parent": s.section.to_string(),
        "body": "P is decided.", "links": {"decided_by": [{"target": d2.to_string()}]}}]),
    )
    .unwrap();
    land(&mut r, "d").unwrap();
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    let d = entry(&c, &d2);
    assert_eq!(
        (d.kind, d.follow_up),
        (ConsequenceKind::Suspect, FollowUp::Reaffirm)
    );
    assert_eq!(d.fix.as_deref(), Some(REAFFIRM_FIX));
    assert_eq!(d.because.edge, "depends_on");
}

/// D-4XTRC: a change does not reach a decision through that decision's own affects link.
#[test]
fn csq1_a_decisions_own_affects_link_gives_no_entry() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d3 = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Scope A".into(),
                affects: vec![s.a.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let d3 = BlockId::parse(&d3.created["$decision"]).unwrap();
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    assert!(!has(&c, &d3), "{c:?}");
}

#[test]
fn csq1_superseding_a_decision_asks_what_it_decided_to_confirm() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let res = supersede_in_w(&mut r, &s, &s.decision);
    let c = after(&r, &res.change);
    let a = entry(&c, &s.a);
    // the superseding decision is proposed: D stays live in the view, so A is not suspect yet
    assert_eq!(
        (a.kind, a.follow_up),
        (ConsequenceKind::Dependent, FollowUp::Confirm)
    );
    assert_eq!(
        a.because,
        Because {
            edge: "decided_by".into(),
            changed: s.decision.to_string(),
            edges: Some(vec!["decided_by".into()]),
            values: Some(vec![]),
            values_omitted: 0,
        }
    );
    assert_eq!(a.fix.as_deref(), Some(SUPERSEDE_CONFIRM_FIX));
}

#[test]
fn csq1_deleting_a_referenced_block_asks_its_referrer_to_update() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // (that a stable delete needs a briefing is Task 3's; this task runs beside it)
    let res = write(
        &mut r,
        "w",
        json!([{"op": "delete", "id": s.a.to_string()}]),
    )
    .unwrap();
    let c = after(&r, &res.change);
    let b = entry(&c, &s.b);
    assert_eq!(
        (b.kind, b.follow_up),
        (ConsequenceKind::Suspect, FollowUp::Update)
    );
}

#[test]
fn csq1_ids_are_deterministic_from_batch_kind_and_target() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let res = update_a(&mut r, &s);
    let change: Hash = res.change.parse().unwrap();
    let id = consequence_id(&change, "suspect", &s.b);
    assert!(
        id.starts_with("cq-") && id.len() == 19 && id[3..].chars().all(|c| c.is_ascii_hexdigit()),
        "{id}"
    );
    assert_eq!(consequence_id(&change, "suspect", &s.b), id);
    assert_ne!(consequence_id(&change, "dependent", &s.b), id);
    assert_ne!(
        consequence_id(&Hash::of(b"another batch"), "suspect", &s.b),
        id
    );
    assert_eq!(entry(&after(&r, &res.change), &s.b).id, id);
    assert_eq!(
        after(&r, &res.change),
        after(&r, &res.change),
        "recomputed, identical"
    );
}

#[test]
fn csq1_a_cut_keeps_counts() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ops: Vec<_> = (0..60)
        .map(|i| {
            json!({"op": "create", "type": "claim", "parent": s.section.to_string(),
                        "body": format!("R{i} cites {{{{{}}}}}.", s.a)})
        })
        .collect();
    write(&mut r, "w", json!(ops)).unwrap();
    let res = update_a(&mut r, &s);
    let full = after(&r, &res.change);
    assert_eq!(full.items.len(), 61, "B and the sixty referrers");
    let cut = r
        .read()
        .consequences(&w(), &res.change.parse().unwrap(), Some(200))
        .unwrap();
    assert!(cut.items.len() < 61);
    assert_eq!(cut.items.len() + cut.omitted.values().sum::<usize>(), 61);
    assert_eq!(
        cut.items[..],
        full.items[..cut.items.len()],
        "a cut drops from the end"
    );
}

#[test]
fn csq1_a_stream_of_new_blocks_has_no_consequences() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let res = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": s.section.to_string(), "body": "New."}]),
    )
    .unwrap();
    assert_eq!(after(&r, &res.change), Consequences::default());
}

#[test]
fn csq1_informative_content_is_asked_to_confirm() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let n = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "note", "id": "$n", "parent": s.section.to_string(),
        "body": format!("See {{{{{}}}}}.", s.a)}]),
    )
    .unwrap();
    let n = BlockId::parse(&n.created["$n"]).unwrap();
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    let e = entry(&c, &n);
    assert_eq!(
        (e.kind, e.follow_up),
        (ConsequenceKind::Suspect, FollowUp::Confirm)
    );
    assert_eq!(e.fix, None);
}

#[test]
fn csq1_a_structural_dependent_is_asked_to_confirm() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let sec = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "section", "id": "$x", "parent": s.doc.to_string(),
        "fields": {"title": format!("About {{{{{}}}}}", s.a)}}]),
    )
    .unwrap();
    let sec = BlockId::parse(&sec.created["$x"]).unwrap();
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    assert_eq!(
        (entry(&c, &sec).kind, entry(&c, &sec).follow_up),
        (ConsequenceKind::Suspect, FollowUp::Confirm)
    );
}

#[test]
fn csq1_a_defining_dependent_is_asked_to_confirm() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let t = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "term", "id": "$t", "parent": s.section.to_string(),
        "fields": {"name": "Alpha"}, "body": format!("Alpha is {{{{{}}}}}.", s.a)}]),
    )
    .unwrap();
    let t = BlockId::parse(&t.created["$t"]).unwrap();
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    assert_eq!(
        (entry(&c, &t).kind, entry(&c, &t).follow_up),
        (ConsequenceKind::Suspect, FollowUp::Confirm)
    );
}

#[test]
fn csq1_a_proposed_decision_dependent_is_asked_to_update() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let p = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Build on A".into(),
                depends_on: vec![s.a.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let p = BlockId::parse(&p.created["$decision"]).unwrap();
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    let e = entry(&c, &p);
    assert_eq!(
        (e.kind, e.follow_up),
        (ConsequenceKind::Suspect, FollowUp::Update)
    );
    assert_eq!(e.fix, None);
}

/// D-ATBP6: the superseding decision is only proposed in the view, so X is not suspect on D
/// there: an accepted decision reached without a suspect gets confirm, never a reaffirm that the
/// reaffirm op would refuse.
#[test]
fn csq1_a_decision_depending_on_a_superseded_decision_is_confirm() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let x = accepted_depending_on(&mut r, &s, &[&s.decision]);
    let res = supersede_in_w(&mut r, &s, &s.decision);
    let c = after(&r, &res.change);
    let e = entry(&c, &x);
    assert_eq!(e.follow_up, FollowUp::Confirm);
    assert_eq!(e.fix.as_deref(), Some(REACH_CONFIRM_FIX));
}

#[test]
fn csq1_a_requirement_depending_on_a_superseded_decision_is_confirm() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let q = write(
        &mut r,
        "d",
        json!([{"op": "create", "type": "requirement", "id": "$q", "parent": s.section.to_string(),
        "body": "Q must hold.", "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
    )
    .unwrap();
    let q = BlockId::parse(&q.created["$q"]).unwrap();
    land(&mut r, "d").unwrap();
    let res = supersede_in_w(&mut r, &s, &s.decision);
    let c = after(&r, &res.change);
    let e = entry(&c, &q);
    assert_eq!(e.follow_up, FollowUp::Confirm);
    assert_eq!(e.fix.as_deref(), Some(SUPERSEDE_CONFIRM_FIX));
}

#[test]
fn csq1_a_deleted_target_asks_informative_content_to_update() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let n = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "note", "id": "$n", "parent": s.section.to_string(),
        "body": format!("See {{{{{}}}}}.", s.a)}]),
    )
    .unwrap();
    let n = BlockId::parse(&n.created["$n"]).unwrap();
    let res = write(
        &mut r,
        "w",
        json!([{"op": "delete", "id": s.a.to_string()}]),
    )
    .unwrap();
    let c = after(&r, &res.change);
    // a repin cannot pin to a block that is gone, so confirm could not clear it
    let e = entry(&c, &n);
    assert_eq!(
        (e.kind, e.follow_up),
        (ConsequenceKind::Suspect, FollowUp::Update)
    );
}

#[test]
fn csq1_a_deleted_target_leaves_an_accepted_decision_to_reaffirm() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let x = accepted_depending_on(&mut r, &s, &[&s.a]);
    let res = write(
        &mut r,
        "w",
        json!([{"op": "delete", "id": s.a.to_string()}]),
    )
    .unwrap();
    let c = after(&r, &res.change);
    let e = entry(&c, &x);
    assert_eq!(e.follow_up, FollowUp::Reaffirm);
    assert_eq!(e.fix.as_deref(), Some(REAFFIRM_FIX));
}

#[test]
fn csq1_a_revoked_decision_dependent_has_no_entry() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let x = accepted_depending_on(&mut r, &s, &[&s.a]);
    r.revoke(
        &writer(),
        "w",
        &x.to_string(),
        "not needed",
        &s.section.to_string(),
        None,
    )
    .unwrap();
    land(&mut r, "w").unwrap();
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    assert!(
        !has(&c, &x),
        "a revoked decision asserts nothing live: {c:?}"
    );
    assert!(has(&c, &s.b), "other dependents are still listed");
}

#[test]
fn csq1_a_rejected_decision_dependent_has_no_entry() {
    use rdy_qa::golden::write_as;
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // An approver may write a decision with a non-proposed status; the golden fixture does too.
    let res = write_as(
        &approver(),
        &mut r,
        "w",
        json!([{"op": "create", "type": "decision", "id": "$x", "parent": s.section.to_string(),
            "fields": {"statement": "Build on A", "status": "rejected",
                       "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z",
                       "options": ["yes", "no"]},
            "links": {"depends_on": [{"target": s.a.to_string()}]}}]),
    )
    .unwrap();
    let x = BlockId::parse(&res.created["$x"]).unwrap();
    land(&mut r, "w").unwrap();
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    assert!(
        !has(&c, &x),
        "a rejected decision asserts nothing live: {c:?}"
    );
    assert!(has(&c, &s.b), "other dependents are still listed");
}

#[test]
fn csq1_a_fully_superseded_decision_dependent_has_no_entry() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let x = accepted_depending_on(&mut r, &s, &[&s.a]);
    supersede_in_w(&mut r, &s, &x);
    land(&mut r, "w").unwrap();
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    assert!(
        !has(&c, &x),
        "a superseded decision asserts nothing live: {c:?}"
    );
}

#[test]
fn csq1_a_partially_superseded_decision_dependent_is_reaffirm() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let x = accepted_depending_on(&mut r, &s, &[&s.a]);
    r.supersede(
        &writer(),
        "w",
        &x.to_string(),
        Some("scope".into()),
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Narrow it".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "w").unwrap();
    let res = update_a(&mut r, &s);
    let c = after(&r, &res.change);
    let e = entry(&c, &x);
    assert_eq!(
        e.follow_up,
        FollowUp::Reaffirm,
        "its live part still asserts"
    );
    assert_eq!(e.fix.as_deref(), Some(REAFFIRM_FIX));
}
