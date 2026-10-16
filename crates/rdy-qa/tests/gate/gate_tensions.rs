//! tgat1: the gate refuses a blocking tension only when every side would be in canon.
use proptest::prelude::*;
use rdy_engine::tensions::*;
use rdy_engine::*;
use rdy_model::{BlockId, Hash};
use rdy_model::{EditorialClass, Justification, Role};
use rdy_qa::edit_loop::{brief_of, spec_doc, stable_spec, write_cited_as};
use rdy_qa::fixtures::*;
use rdy_qa::intake::{confirm_before_landing, summary_for};
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::json;
use std::collections::BTreeSet;

/// Proposes `view`, with a summary when it writes a decision (F2: such a version is approved
/// through a card).
fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    let summary = summary_for(r, view);
    r.propose(&writer(), view, view, summary).unwrap()
}
/// Lands `p` as the approver, after the person confirms each of its exceptions (F2).
fn approve(r: &mut Rdy, p: &ProposalInfo) -> Result<String, RdyError> {
    confirm_before_landing(r, &approver(), p);
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
                involved: vec![cx.involved(side, &self.0, Some(v))?],
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

/// F1 (adversarial review, fix round 2): a change that another view also includes stays live
/// there, but what view `b` lands is its resolution, so no collision with canon lands.
#[test]
fn tgat1_a_change_included_in_another_view_lands_with_its_views_resolution() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let cb = change_of(&put(&mut r, "b", upd(&s.a, "A by b.")));
    // a colleague's view o includes b's change
    put(&mut r, "o", upd(&s.b, "B by o."));
    r.view_include(&ag("o"), "o", &[cb.to_string()]).unwrap();
    put(&mut r, "a", upd(&s.a, "A by a."));
    let pa = propose(&mut r, "a");
    approve(&mut r, &pa).unwrap();
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
    assert!(pb.blocking.is_empty(), "{:?}", pb.blocking);
    assert!(
        pb.tensions.iter().all(|t| t.refuses == Some(false)),
        "{:?}",
        pb.tensions
    );
    approve(&mut r, &pb).unwrap();
}

/// F3 (adversarial review, fix round 2): a subset that leaves out the repin lands the stale pin,
/// so the gate refuses it although the view as a whole has no tension.
#[test]
fn tgat1_a_subset_that_leaves_out_the_repin_refuses() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let w1 = put(&mut r, "b", cite(&s.section, &s.a));
    let y = created(&w1, "$y");
    let c1 = change_of(&w1);
    put(&mut r, "a", upd(&s.a, "A changed."));
    let pa = propose(&mut r, "a");
    approve(&mut r, &pa).unwrap();
    put(&mut r, "b", json!([{"op": "repin", "id": y.to_string()}]));
    let whole = propose(&mut r, "b");
    assert!(
        whole.tensions.iter().all(|t| t.refuses == Some(false)),
        "the whole view repins: {:?}",
        whole.tensions
    );
    let p = r
        .propose_subset(&writer(), "b", "b", Some(&[c1.to_string()]), None)
        .unwrap();
    assert!(
        p.tensions
            .iter()
            .any(|t| t.kind == TensionKind::PremiseShift && t.refuses == Some(true)),
        "{:?}",
        p.tensions
    );
    let e = approve(&mut r, &p).unwrap_err();
    assert_eq!(e.code, E_BLOCKING_TENSION, "{e:?}");
}

/// One step of a gate scenario: (kind, view, target, n). Kinds: 0 edit a claim, 1 cite one,
/// 2 repin the view's last citer, 3 supersede the decision, 4 include another view's change,
/// 5–6 set the section's lifecycle (draft or stable), 7–8 define a term, 9 dismiss a tension as
/// the view's own agent, 10 dismiss one as a reviewer, 11–13 propose (the whole view, or its
/// first n changes) and land.
type Step = (u8, u8, u8, u8);

/// A write as view `vn`'s agent, citing a fresh briefing of canon's spec section and claims (so
/// a write in a stable section is accepted).
fn wr(
    r: &mut Rdy,
    s: &rdy_qa::edit_loop::Spec,
    vn: &str,
    ops: serde_json::Value,
) -> Result<rdy_engine::WriteResult, RdyError> {
    let tok = brief_of(r, &Scope::Canon, &[&s.section, &s.a, &s.b, &s.c])
        .base
        .token;
    write_cited_as(r, &ag(vn), vn, ops, Some(&tok))
}

/// Runs `steps` on a stable spec project (so lifecycles mix: canon stable, a view's draft). At each propose step: `propose` reports a refusing tension
/// exactly when `land` refuses with `E_BLOCKING_TENSION`; every refusing entry is listed in the
/// form that refuses (open and blocking); and a reviewer (an actor that wrote no side) can
/// dismiss each one, after which it no longer refuses.
fn propose_agrees_with_land(steps: Vec<Step>) -> Result<(), TestCaseError> {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let names = ["a", "b", "o"];
    let claims = [s.a.clone(), s.b.clone(), s.c.clone()];
    // the citing claims each view created, newest last
    let mut citers: Vec<Vec<BlockId>> = vec![vec![]; 3];
    // the ids of refusing tensions seen at a propose, newest last
    let mut refused: Vec<String> = vec![];
    for (i, (kind, v, k, n)) in steps.into_iter().enumerate() {
        let vn = names[v as usize];
        let target = &claims[k as usize % 3];
        match kind {
            0 => {
                let _ = wr(&mut r, &s, vn, upd(target, &format!("{vn} {i}")));
            }
            1 => {
                if let Ok(w) = wr(&mut r, &s, vn, cite(&s.section, target)) {
                    citers[v as usize].push(created(&w, "$y"));
                }
            }
            2 => {
                if let Some(y) = citers[v as usize].last().cloned() {
                    let _ = wr(
                        &mut r,
                        &s,
                        vn,
                        json!([{"op": "repin", "id": y.to_string()}]),
                    );
                }
            }
            3 => {
                let _ = r.supersede(
                    &ag(vn),
                    vn,
                    &s.decision.to_string(),
                    None,
                    DecisionInput {
                        parent: s.section.to_string(),
                        statement: format!("s{i}"),
                        ..Default::default()
                    },
                );
            }
            4 => {
                let from = names[k as usize % 3];
                let inc = r.read().included(from);
                if let Some(c) = inc.get(n as usize % inc.len().max(1)) {
                    let _ = r.view_include(&ag(vn), vn, &[c.to_string()]);
                }
            }
            5 | 6 => {
                let lifecycle = if k % 2 == 0 { "draft" } else { "stable" };
                let _ = wr(
                    &mut r,
                    &s,
                    vn,
                    json!([{"op": "update", "id": s.section.to_string(), "fields": {"lifecycle": lifecycle}}]),
                );
            }
            7 | 8 => {
                let _ = wr(
                    &mut r,
                    &s,
                    vn,
                    term(&s.section, ["Fee", "fee", "Charge"][k as usize % 3]),
                );
            }
            9 | 10 => {
                // a tension of the in-flight set, or one a propose refused with
                let mut ids: Vec<String> = open(&r).iter().map(|t| tension_label(&t.id)).collect();
                ids.extend(refused.iter().cloned());
                if ids.is_empty() {
                    continue;
                }
                let id = ids[(n as usize + k as usize) % ids.len()].clone();
                let who = if kind == 9 { ag(vn) } else { reviewer() };
                let _ = r.dismiss_tension(&who, &id, "fine");
            }
            _ => {
                let inc = r.read().included(vn);
                if inc.is_empty() {
                    continue;
                }
                // n == 0: the whole view; else its first n changes (a subset that can be
                // refused as not closed, which proposes nothing)
                let propose = |r: &mut Rdy| {
                    if n == 0 {
                        r.propose(&writer(), vn, vn, None)
                    } else {
                        let ids: Vec<String> = inc
                            .iter()
                            .take(n as usize)
                            .map(|h: &Hash| h.to_string())
                            .collect();
                        r.propose_subset(&writer(), vn, vn, Some(&ids), None)
                    }
                };
                let Ok(p) = propose(&mut r) else { continue };
                let refusing: Vec<&TensionInfo> = p
                    .tensions
                    .iter()
                    .filter(|t| t.refuses == Some(true))
                    .collect();
                for t in &refusing {
                    prop_assert!(
                        t.status == "open" && t.severity == TensionSeverity::Blocking,
                        "step {}: a refusing entry in another form: {:?}",
                        i,
                        t
                    );
                }
                refused.extend(refusing.iter().map(|t| t.id.clone()));
                let landed = r.land(&approver(), &p.id);
                let gate = matches!(&landed, Err(e) if e.code == E_BLOCKING_TENSION);
                if !p.blocking.is_empty() {
                    prop_assert!(!gate, "a blocking conflict refuses first: {landed:?}");
                    continue;
                }
                prop_assert_eq!(
                    !refusing.is_empty(),
                    gate,
                    "step {}: propose {:?}, land {:?}",
                    i,
                    p.tensions,
                    landed.as_ref().map_err(|e| (e.code, e.message.clone()))
                );
                if gate {
                    // a refusal is always clearable by a reviewer of another actor
                    for t in &refusing {
                        let d = r.dismiss_tension(&reviewer(), &t.id, "both fine");
                        prop_assert!(
                            d.is_ok(),
                            "step {}: dismiss {}: {:?}",
                            i,
                            t.id,
                            d.map_err(|e| (e.code, e.message))
                        );
                    }
                    let Ok(p2) = propose(&mut r) else { continue };
                    for t in &refusing {
                        prop_assert!(
                            !p2.tensions
                                .iter()
                                .any(|x| x.id == t.id && x.refuses == Some(true)),
                            "step {}: {} still refuses after its dismissal: {:?}",
                            i,
                            t.id,
                            p2.tensions
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(32), ..ProptestConfig::default() })]
    #[test]
    fn tgat1_propose_reports_a_refusal_iff_land_refuses_with_a_blocking_tension(
        steps in proptest::collection::vec((0u8..14, 0u8..3, 0u8..3, 0u8..3), 4..20)
    ) {
        propose_agrees_with_land(steps)?;
    }
}

/// N1 (re-review, fix round 3): the in-flight form of a duplicate definition is advisory (view
/// o makes the section draft in flight), but the landing form is blocking (b lands into canon's
/// stable section). The landing form is listed as itself, b's own dismissal of the advisory
/// form does not clear it, and a reviewer's dismissal of it lets b land.
#[test]
fn tgat1_a_landing_form_with_its_own_severity_is_listed_and_dismissable() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let tok = |r: &Rdy| brief_of(r, &Scope::Canon, &[&s.section]).base.token;
    let t0 = tok(&r);
    write_cited_as(&mut r, &ag("a"), "a", term(&s.section, "Fee"), Some(&t0)).unwrap();
    let pa = propose(&mut r, "a");
    approve(&mut r, &pa).unwrap();
    let t1 = tok(&r);
    write_cited_as(
        &mut r,
        &ag("o"),
        "o",
        json!([{"op": "update", "id": s.section.to_string(), "fields": {"lifecycle": "draft"}}]),
        Some(&t1),
    )
    .unwrap();
    let t2 = tok(&r);
    write_cited_as(&mut r, &ag("b"), "b", term(&s.section, "fee"), Some(&t2)).unwrap();
    let inflight = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(inflight.severity, TensionSeverity::Advisory);
    r.dismiss_tension(&ag("b"), &tension_label(&inflight.id), "fine")
        .unwrap();
    let pb = propose(&mut r, "b");
    let refusing: Vec<&TensionInfo> = pb
        .tensions
        .iter()
        .filter(|t| t.refuses == Some(true))
        .collect();
    assert_eq!(refusing.len(), 1, "{:?}", pb.tensions);
    let landing = refusing[0];
    assert_eq!(landing.kind, TensionKind::DuplicateDefinition);
    assert_eq!(landing.severity, TensionSeverity::Blocking);
    assert_eq!(landing.status, "open");
    assert_ne!(landing.id, tension_label(&inflight.id), "its own form");
    assert!(
        pb.tensions
            .iter()
            .any(|t| t.id == tension_label(&inflight.id)
                && t.refuses == Some(false)
                && t.status == "dismissed"),
        "the in-flight form is listed as information: {:?}",
        pb.tensions
    );
    assert_eq!(approve(&mut r, &pb).unwrap_err().code, E_BLOCKING_TENSION);
    // b's own session cannot clear a blocking form
    assert!(r.dismiss_tension(&ag("b"), &landing.id, "fine").is_err());
    r.dismiss_tension(&reviewer(), &landing.id, "both fine")
        .unwrap();
    let pb = propose(&mut r, "b");
    assert!(
        pb.tensions.iter().all(|t| t.refuses == Some(false)),
        "{:?}",
        pb.tensions
    );
    approve(&mut r, &pb).unwrap();
}

/// A pack whose decisions carry the tracking field `disposition` (trk1).
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
    tracking: [disposition]
";

/// N2 (re-review, fix round 3): a fork that exists only in its landing form (view o makes a
/// tracking edit of the canon decision in flight, so the in-flight fork is against o's version)
/// is listed, and a reviewer dismisses it by the id the refusal names.
#[test]
fn tgat1_a_landing_only_fork_is_dismissable_by_its_id() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let s = spec_doc(&mut r);
    let supersede = |r: &mut Rdy, who: &str| {
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
        .unwrap()
    };
    let da = supersede(&mut r, "a").created["$decision"].clone();
    let pa = propose(&mut r, "a");
    approve(&mut r, &pa).unwrap();
    // o makes a tracking edit of a's landed decision in flight
    r.write(
        &ag("o"),
        WriteRequest {
            view: "o".into(),
            ops: serde_json::from_value(
                json!([{"op": "update", "id": da, "fields": {"disposition": "M3"}}]),
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
    supersede(&mut r, "b");
    let pb = propose(&mut r, "b");
    let refusing: Vec<&TensionInfo> = pb
        .tensions
        .iter()
        .filter(|t| t.refuses == Some(true))
        .collect();
    assert_eq!(refusing.len(), 1, "{:?}", pb.tensions);
    assert_eq!(refusing[0].kind, TensionKind::Fork);
    assert!(
        open(&r)
            .iter()
            .all(|t| tension_label(&t.id) != refusing[0].id),
        "a landing-only form"
    );
    let id = refusing[0].id.clone();
    assert_eq!(approve(&mut r, &pb).unwrap_err().code, E_BLOCKING_TENSION);
    r.dismiss_tension(&reviewer(), &id, "both stand").unwrap();
    let pb = propose(&mut r, "b");
    approve(&mut r, &pb).unwrap();
}

/// Final review X1 (ledger, Task 13 note): a tension the gate refuses with that exists only in
/// its landing form is listed by `rdy_tensions`, marked with its proposal, and the listing the
/// refusal names (`involving` a change of the version) shows it.
#[test]
fn tgat1_a_landing_only_form_is_listed_with_its_proposal() {
    use rdy_engine::tensions::api::{TensionsRequest, run_tensions};
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let tok = |r: &Rdy| brief_of(r, &Scope::Canon, &[&s.section]).base.token;
    let t0 = tok(&r);
    write_cited_as(&mut r, &ag("a"), "a", term(&s.section, "Fee"), Some(&t0)).unwrap();
    let pa = propose(&mut r, "a");
    approve(&mut r, &pa).unwrap();
    let t1 = tok(&r);
    write_cited_as(
        &mut r,
        &ag("o"),
        "o",
        json!([{"op": "update", "id": s.section.to_string(), "fields": {"lifecycle": "draft"}}]),
        Some(&t1),
    )
    .unwrap();
    let t2 = tok(&r);
    let wb = write_cited_as(&mut r, &ag("b"), "b", term(&s.section, "fee"), Some(&t2)).unwrap();
    let cb = change_of(&wb);
    let pb = propose(&mut r, "b");
    let refusing: Vec<String> = pb
        .tensions
        .iter()
        .filter(|t| t.refuses == Some(true))
        .map(|t| t.id.clone())
        .collect();
    assert_eq!(refusing.len(), 1, "{:?}", pb.tensions);
    assert!(
        open(&r).iter().all(|t| tension_label(&t.id) != refusing[0]),
        "a landing-only form"
    );
    let e = approve(&mut r, &pb).unwrap_err();
    assert_eq!(e.code, E_BLOCKING_TENSION);
    assert!(
        e.fix.contains("rdy tensions --involving <change>"),
        "{}",
        e.fix
    );
    let engine = Local::new(r);
    let listed = |req: TensionsRequest| -> Vec<serde_json::Value> {
        run_tensions(&engine, &writer(), req).unwrap()["tensions"]
            .as_array()
            .unwrap()
            .clone()
    };
    let all = listed(TensionsRequest::default());
    let row = all
        .iter()
        .find(|t| t["id"] == refusing[0].as_str())
        .unwrap_or_else(|| panic!("the landing form is listed: {all:#?}"));
    assert_eq!(row["proposal"], pb.r#ref.as_str(), "{row:#?}");
    assert_eq!(row["severity"], "blocking");
    let involving = listed(TensionsRequest {
        involving: Some(cb.to_string()[..12].to_string()),
        ..Default::default()
    });
    assert!(
        involving.iter().any(|t| t["id"] == refusing[0].as_str()),
        "{involving:#?}"
    );
    // the in-flight forms carry no proposal
    assert!(
        all.iter()
            .filter(|t| t["id"] != refusing[0].as_str())
            .all(|t| t.get("proposal").is_none()),
        "{all:#?}"
    );
}

/// Perf fix (mcp_write_9mib): a version with thousands of tensions builds the sides of only the
/// `PROPOSAL_TENSIONS_MAX` it lists; the rest are counted in `tensions_omitted`.
#[test]
fn tgat1_a_version_with_thousands_of_tensions_builds_only_the_listed_ones() {
    use rdy_engine::tensions::info::info_counts;
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // 2,000 pairs of terms with one name each: 2,000 duplicate groups (FW-TN2)
    let mut ops = vec![];
    for i in 0..4000 {
        ops.push(
            json!({"op": "create", "type": "term", "id": format!("$f{i}"),
            "parent": s.section.to_string(), "fields": {"name": format!("fee {}", i / 2)},
            "body": format!("Fee number {i} is a term.")}),
        );
    }
    put(&mut r, "b", serde_json::Value::Array(ops));
    let (_, b0) = info_counts();
    let p = propose(&mut r, "b");
    let (_, b1) = info_counts();
    assert_eq!(p.tensions.len(), PROPOSAL_TENSIONS_MAX);
    assert_eq!(p.tensions.len() + p.tensions_omitted, 2000);
    let mut ids: Vec<&String> = p.tensions.iter().map(|t| &t.tension).collect();
    ids.sort();
    assert_eq!(
        ids,
        p.tensions.iter().map(|t| &t.tension).collect::<Vec<_>>(),
        "by id"
    );
    assert!(
        b1 - b0 <= PROPOSAL_TENSIONS_MAX as u64,
        "{} rows built",
        b1 - b0
    );
}

/// FW-TN2: the landing form of a duplicate group is the group over canon and the landing set. A
/// member in flight elsewhere is not in it; the all-canon group alone never refused. A landed
/// member changes the group, so the next landing's form has a new id (the dismissal is void).
#[test]
fn tgat1_a_group_with_one_member_in_the_proposal_refuses_only_when_all_would_be_in_canon() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let mut canon = vec![];
    for (v, n) in [("x", "Fee"), ("y", "fee")] {
        canon.push(created(&put(&mut r, v, term(&s.section, n)), "$t"));
        land(&mut r, v).unwrap();
    }
    let tok = brief_of(&r, &Scope::Canon, &[&s.section]).base.token;
    write_cited_as(
        &mut r,
        &writer(),
        "st",
        json!([{"op": "update", "id": s.section.to_string(), "fields": {"lifecycle": "stable"}}]),
        Some(&tok),
    )
    .unwrap();
    land(&mut r, "st").unwrap();
    let standing = one(&r, TensionKind::DuplicateDefinition);
    assert!(change_sides(&standing).is_empty(), "{standing:#?}");
    assert_eq!(standing.severity, TensionSeverity::Blocking);
    let tok = brief_of(&r, &Scope::Canon, &[&s.section]).base.token;
    let b = created(
        &write_cited_as(&mut r, &ag("b"), "b", term(&s.section, "FEE"), Some(&tok)).unwrap(),
        "$t",
    );
    let c = created(
        &write_cited_as(&mut r, &ag("c"), "c", term(&s.section, "fEE"), Some(&tok)).unwrap(),
        "$t",
    );
    let refusing = |p: &ProposalInfo| -> Vec<TensionInfo> {
        p.tensions
            .iter()
            .filter(|t| t.refuses == Some(true))
            .cloned()
            .collect()
    };
    let pb = propose(&mut r, "b");
    let rb = refusing(&pb);
    assert_eq!(rb.len(), 1, "{:?}", pb.tensions);
    let mut want: Vec<String> = [&canon[0], &canon[1], &b]
        .iter()
        .map(|x| x.to_string())
        .collect();
    want.sort();
    assert_eq!(rb[0].blocks, want, "c is in flight elsewhere: not a member");
    assert_eq!(approve(&mut r, &pb).unwrap_err().code, E_BLOCKING_TENSION);
    r.dismiss_tension(&reviewer(), &rb[0].id, "one meaning")
        .unwrap();
    let pb = propose(&mut r, "b");
    approve(&mut r, &pb).unwrap();
    let pc = propose(&mut r, "c");
    let rc = refusing(&pc);
    assert_eq!(rc.len(), 1, "{:?}", pc.tensions);
    assert_eq!(rc[0].blocks.len(), 4);
    assert_ne!(rc[0].id, rb[0].id);
    assert_eq!(
        rc[0].status, "open",
        "the membership changed: the dismissal is void"
    );
    assert_eq!(approve(&mut r, &pc).unwrap_err().code, E_BLOCKING_TENSION);
    assert!(rc[0].blocks.contains(&c.to_string()), "{:?}", rc[0].blocks);
}

/// Perf fix (pre-merge, propose_land_500_flat): a proposal made right after a landing lists the
/// tensions of its changes without building the whole set (canon's part included), and lists
/// the ones the built set has.
#[test]
fn tgat1_a_proposal_after_a_landing_builds_no_whole_tension_set() {
    use rdy_engine::tensions::set::full_builds;
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "b", cite(&s.section, &s.a));
    put(&mut r, "a", upd(&s.a, "A changed."));
    put(&mut r, "c", upd(&s.c, "C changed."));
    let pc = propose(&mut r, "c");
    approve(&mut r, &pc).unwrap();
    let before = full_builds();
    let pb = propose(&mut r, "b");
    assert_eq!(full_builds(), before, "no whole set was built");
    assert_eq!(pb.tensions.len(), 1, "{:?}", pb.tensions);
    assert_eq!(pb.tensions[0].kind, TensionKind::PremiseShift);
    let st = r.read();
    let set = st.tensions().unwrap();
    let from_set: BTreeSet<String> = st
        .included("b")
        .iter()
        .flat_map(|c| set.involving_change(c))
        .map(|t| t.id.to_string())
        .collect();
    let listed: BTreeSet<String> = pb.tensions.iter().map(|t| t.tension.clone()).collect();
    assert_eq!(listed, from_set);
}

/// FW-JU1 at propose (FW-LP3): the proposal's net change is judged. A subset that leaves out
/// the justified rewrite lists its landing form; the whole view lists none.
#[test]
fn msw21_propose_judges_the_proposals_net_change() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let c1 = change_of(&put_j(&mut r, "a", upd(&s.c, "C changed."), None));
    put_j(
        &mut r,
        "a",
        upd(&s.c, "C, as D says."),
        j_implements(&s.decision),
    );
    let sides = |p: &ProposalInfo| -> Vec<Vec<String>> {
        p.tensions
            .iter()
            .filter(|t| t.kind == TensionKind::UnjustifiedChange)
            .map(|t| t.sides.iter().map(|x| x.side.clone()).collect())
            .collect()
    };
    let sub = r
        .propose_with(&writer(), "a", "a", Some(&[c1.to_string()]), None, None)
        .unwrap();
    assert_eq!(
        sides(&sub),
        vec![vec![c1.to_string()]],
        "{:?}",
        sub.tensions
    );
    assert!(sub.tensions.iter().all(|t| t.refuses == Some(false)));
    let full = propose(&mut r, "a");
    assert!(sides(&full).is_empty(), "{:?}", full.tensions);
}

/// FW-JU1, the landing: the gate judges the landing set's net change. A justified rewrite after
/// an unjustified edit lands; a decision lands with no justification; a subset that leaves out
/// the justified rewrite refuses (the qa pack makes the kind blocking).
#[test]
fn msw21_the_landing_judges_the_landing_sets_net_change() {
    let (_d, mut r) = qa_project();
    let s = stable_spec(&mut r);
    put_j(&mut r, "a", upd(&s.c, "C changed."), None);
    put_j(&mut r, "a", upd(&s.c, "C, typo fixed."), j_typo());
    let pa = propose(&mut r, "a");
    assert!(
        pa.tensions.iter().all(|t| t.refuses != Some(true)),
        "{:?}",
        pa.tensions
    );
    approve(&mut r, &pa).unwrap();
    try_decide_in(&mut r, "d", &s.section, "Keep C short").unwrap();
    let pd = propose(&mut r, "d");
    approve(&mut r, &pd).unwrap();
    let c5 = change_of(&put_j(&mut r, "o", upd(&s.c, "C changed in o."), None));
    put_j(
        &mut r,
        "o",
        upd(&s.c, "C, as D says."),
        j_implements(&s.decision),
    );
    let po = r
        .propose_with(&writer(), "o", "o", Some(&[c5.to_string()]), None, None)
        .unwrap();
    assert_eq!(approve(&mut r, &po).unwrap_err().code, E_BLOCKING_TENSION);
}
