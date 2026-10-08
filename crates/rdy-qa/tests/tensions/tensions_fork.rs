//! tfrk1: fork: two live or in-flight decisions supersede the same decision, or the same part.
use rdy_engine::tensions::*;
use rdy_engine::{DecisionInput, Rdy, WriteResult};
use rdy_model::BlockId;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::json;

fn sup(r: &mut Rdy, who: &str, old: &BlockId, part: Option<&str>, parent: &BlockId) -> WriteResult {
    r.supersede(
        &ag(who),
        who,
        &old.to_string(),
        part.map(str::to_string),
        DecisionInput {
            parent: parent.to_string(),
            statement: format!("{who} replaces it"),
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn tfrk1_two_in_flight_decisions_superseding_one_decision_fork() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ca = change_of(&sup(&mut r, "a", &s.decision, None, &s.section));
    let cb = change_of(&sup(&mut r, "b", &s.decision, None, &s.section));
    let t = one(&r, TensionKind::Fork);
    let mut want = vec![ca, cb];
    want.sort();
    assert_eq!(change_sides(&t), want);
    assert_eq!(t.severity, TensionSeverity::Blocking);
    assert!(t.summary.contains(&s.decision.to_string()), "{}", t.summary);
}

#[test]
fn tfrk1_after_one_lands_the_other_forks_against_canon_until_it_supersedes_the_new_one() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let wa = sup(&mut r, "a", &s.decision, None, &s.section);
    let wb = sup(&mut r, "b", &s.decision, None, &s.section);
    let before = one(&r, TensionKind::Fork);
    land(&mut r, "a").unwrap();
    let t = one(&r, TensionKind::Fork);
    assert!(has_canon(&t));
    assert_eq!(t.vkey, before.vkey, "re-keyed with the same vkey (FW-CH6a)");
    let (da, db) = (created(&wa, "$decision"), created(&wb, "$decision"));
    // b now supersedes a's decision instead (if the write path refuses this links edit on a
    // proposed decision, supersede `da` from b with a new decision and exclude `wb`'s change,
    // and say so in the report)
    put(
        &mut r,
        "b",
        json!([{"op": "update", "id": db.to_string(),
        "links": {"supersedes": [{"target": da.to_string()}]}}]),
    );
    assert!(of_kind(&r, TensionKind::Fork).is_empty());
}

#[test]
fn tfrk1_different_parts_do_not_fork_and_a_whole_supersede_forks_with_each_part() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    sup(&mut r, "a", &s.decision, Some("scope"), &s.section);
    sup(&mut r, "b", &s.decision, Some("cost"), &s.section);
    assert!(of_kind(&r, TensionKind::Fork).is_empty());
    sup(&mut r, "c", &s.decision, None, &s.section);
    assert_eq!(of_kind(&r, TensionKind::Fork).len(), 2);
}
