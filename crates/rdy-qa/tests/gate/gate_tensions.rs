//! tgat1: the gate refuses a blocking tension only when every side would be in canon.
use rdy_engine::tensions::*;
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_model::{EditorialClass, Justification, Role};
use rdy_qa::edit_loop::{brief_of, spec_doc, stable_spec, write_cited_as};
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::json;
use std::collections::BTreeSet;

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    r.propose(&writer(), view, view).unwrap()
}
fn approve(r: &mut Rdy, p: &ProposalInfo) -> Result<String, RdyError> {
    r.land(&approver(), &p.id)
}
fn reviewer() -> Session {
    Session::agent("agent:r".into(), Some("rr".into()), Role::Reviewer).unwrap()
}

#[test]
fn tgat1_a_shift_between_two_views_refuses_neither_and_the_second_refuses_after_the_first_lands_until_it_repins()
 {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let y = created(&put(&mut r, "b", cite(&s.section, &s.a)), "$y");
    put(&mut r, "a", upd(&s.a, "A changed."));
    let pa = propose(&mut r, "a");
    let pb = propose(&mut r, "b");
    for p in [&pa, &pb] {
        assert_eq!(p.tensions.len(), 1, "{:?}", p.tensions);
        assert_eq!(p.tensions[0].kind, TensionKind::PremiseShift);
        assert_eq!(p.tensions[0].refuses, Some(false));
    }
    approve(&mut r, &pa).unwrap();
    let pb = propose(&mut r, "b");
    assert_eq!(pb.tensions[0].refuses, Some(true));
    let e = approve(&mut r, &pb).unwrap_err();
    assert_eq!(e.code, E_BLOCKING_TENSION);
    assert!(
        e.issues
            .iter()
            .any(|i| i.code == E_BLOCKING_TENSION && i.message.contains(&pb.tensions[0].id)),
        "{e:?}"
    );
    put(&mut r, "b", json!([{"op": "repin", "id": y.to_string()}]));
    let pb = propose(&mut r, "b");
    assert!(pb.tensions.is_empty(), "{:?}", pb.tensions);
    approve(&mut r, &pb).unwrap();
}

#[test]
fn tgat1_two_colliding_views_land_one_after_the_other_and_the_collision_is_reported_once_as_a_conflict()
 {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let pa = propose(&mut r, "a");
    approve(&mut r, &pa).unwrap();
    let pb = propose(&mut r, "b");
    assert!(!pb.blocking.is_empty(), "the rebase's conflict");
    assert!(
        pb.tensions
            .iter()
            .all(|t| t.kind != TensionKind::EditCollision),
        "reported once, as the conflict"
    );
    assert_eq!(
        approve(&mut r, &pb).unwrap_err().code,
        E_CONFLICT_UNRESOLVED
    );
    // a write on a conflicted block names the version it edited: the conflict's base
    let base = r
        .read()
        .conflict_on(&Scope::View("b".into()), &s.a)
        .unwrap()
        .unwrap()
        .base
        .unwrap();
    put(
        &mut r,
        "b",
        json!([{"op": "update", "id": s.a.to_string(), "base": base.to_string(), "body": "A merged."}]),
    );
    let pb = propose(&mut r, "b");
    approve(&mut r, &pb).unwrap();
}

#[test]
fn tgat1_two_forking_decisions_in_one_proposal_refuse() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    for i in 0..2 {
        r.supersede(
            &ag("a"),
            "a",
            &s.decision.to_string(),
            None,
            DecisionInput {
                parent: s.section.to_string(),
                statement: format!("Replace it {i}"),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let pa = propose(&mut r, "a");
    assert!(
        pa.tensions
            .iter()
            .any(|t| t.kind == TensionKind::Fork && t.refuses == Some(true))
    );
    assert_eq!(approve(&mut r, &pa).unwrap_err().code, E_BLOCKING_TENSION);
}

#[test]
fn tgat1_a_dismissal_lets_the_landing_through_and_survives_the_rekey() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let y = created(&put(&mut r, "b", cite(&s.section, &s.a)), "$y");
    put(&mut r, "a", upd(&s.a, "A changed."));
    let t = one_involving(&r, TensionKind::PremiseShift, &y);
    r.dismiss_tension(&reviewer(), &tension_label(&t.id), "B still holds")
        .unwrap();
    let pa = propose(&mut r, "a");
    approve(&mut r, &pa).unwrap();
    // one side landed, one in flight: re-keyed (same vkey), still dismissed (FW-CH7)
    let t2 = one_involving(&r, TensionKind::PremiseShift, &y);
    assert_ne!(t2.id, t.id);
    assert_eq!(t2.vkey, t.vkey);
    assert!(matches!(
        r.read().tension_status(&t2),
        TensionStatus::Dismissed(_)
    ));
    let pb = propose(&mut r, "b");
    assert_eq!(pb.tensions.len(), 1, "{:?}", pb.tensions);
    assert_eq!(pb.tensions[0].refuses, Some(false));
    assert_eq!(pb.tensions[0].status, "dismissed");
    approve(&mut r, &pb).unwrap();
    // FW-CH6a: the last side landed: resolved, not re-keyed; the stale pin is a suspect
    assert!(
        of_kind(&r, TensionKind::PremiseShift)
            .iter()
            .all(|x| !x.blocks().contains(&y))
    );
    let sus = r.read().suspects(&Scope::Canon).unwrap();
    assert!(
        sus.iter().any(|x| x.block == y && x.target == s.a),
        "{sus:#?}"
    );
}

#[test]
fn tgat1_advisory_tensions_never_refuse() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", term(&s.section, "Fee"));
    put(&mut r, "a", term(&s.section, "fee"));
    assert_eq!(
        one(&r, TensionKind::DuplicateDefinition).severity,
        TensionSeverity::Advisory
    );
    let pa = propose(&mut r, "a");
    approve(&mut r, &pa).unwrap();
}

#[test]
fn tgat1_an_unjustified_change_refuses_when_the_pack_makes_it_blocking() {
    let (_d, mut r) = qa_project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.c]).base.token;
    write_cited_as(&mut r, &ag("a"), "a", upd(&s.c, "C changed."), Some(&tok)).unwrap();
    let pa = propose(&mut r, "a");
    assert!(
        pa.tensions
            .iter()
            .any(|t| t.kind == TensionKind::UnjustifiedChange && t.refuses == Some(true))
    );
    assert_eq!(approve(&mut r, &pa).unwrap_err().code, E_BLOCKING_TENSION);
    let ops: Vec<OpInput> = serde_json::from_value(upd(&s.c, "C, typo fixed.")).unwrap();
    r.write_briefed(
        &ag("j"),
        WriteRequest {
            view: "j".into(),
            ops,
            justification: Some(Justification::Editorial {
                class: EditorialClass::Typo,
                reason: "typo".into(),
            }),
            message: String::new(),
        },
        Some(tok),
    )
    .unwrap();
    let pj = propose(&mut r, "j");
    assert!(
        pj.tensions.iter().all(|t| t.refuses != Some(true)),
        "{:?}",
        pj.tensions
    );
    approve(&mut r, &pj).unwrap();
}

/// In the canon scope only: a blocking fork on block `0` alone, with its canon side.
struct CanonOnlyFork(BlockId);
impl Detector for CanonOnlyFork {
    fn kind(&self) -> TensionKind {
        TensionKind::Fork
    }
    fn detect(
        &self,
        cx: &DetectCx<'_>,
        _seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        if cx.scope() != DetectScope::Canon {
            return Ok(());
        }
        if let Some((side, v)) = cx.canon_side(&self.0)? {
            out.push(Found {
                kind: self.kind(),
                involved: vec![Involved {
                    side,
                    block: self.0.clone(),
                    version: Some(v),
                }],
                summary: "canon only".into(),
                stable: true,
            });
        }
        Ok(())
    }
}

#[test]
fn tgat1_a_tension_whose_sides_are_all_canon_never_refuses_a_landing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let det: &'static dyn Detector = Box::leak(Box::new(CanonOnlyFork(s.a.clone())));
    r.set_detectors_for_tests(Box::leak(vec![det].into_boxed_slice()));
    let t = one(&r, TensionKind::Fork);
    assert!(change_sides(&t).is_empty() && has_canon(&t), "{t:#?}");
    assert_eq!(t.severity, TensionSeverity::Blocking);
    // a landing that writes the tension's own block: no side of the tension is in the proposal
    put(&mut r, "c", upd(&s.a, "A changed."));
    let pc = propose(&mut r, "c");
    assert!(pc.tensions.is_empty(), "{:?}", pc.tensions);
    approve(&mut r, &pc).unwrap();
    let t2 = one(&r, TensionKind::Fork);
    assert_ne!(t2.id, t.id, "canon's A moved: the canon part is rebuilt");
}

#[test]
fn tgat1_a_dismissed_fork_lands_and_stays_open_in_canon() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    for who in ["a", "b"] {
        r.supersede(
            &ag(who),
            who,
            &s.decision.to_string(),
            None,
            DecisionInput {
                parent: s.section.to_string(),
                statement: format!("{who} replaces it"),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let pa = propose(&mut r, "a");
    approve(&mut r, &pa).unwrap();
    let t = one(&r, TensionKind::Fork);
    let pb = propose(&mut r, "b");
    assert!(
        pb.tensions
            .iter()
            .any(|x| x.kind == TensionKind::Fork && x.refuses == Some(true))
    );
    assert_eq!(approve(&mut r, &pb).unwrap_err().code, E_BLOCKING_TENSION);
    r.dismiss_tension(&reviewer(), &tension_label(&t.id), "both stand")
        .unwrap();
    let pb = propose(&mut r, "b");
    approve(&mut r, &pb).unwrap();
    let t2 = one(&r, TensionKind::Fork);
    assert!(change_sides(&t2).is_empty() && has_canon(&t2), "{t2:#?}");
    assert_eq!(t2.vkey, t.vkey, "re-keyed, not resolved");
    assert!(matches!(
        r.read().tension_status(&t2),
        TensionStatus::Dismissed(_)
    ));
}
