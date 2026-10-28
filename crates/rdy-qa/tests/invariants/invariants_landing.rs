//! sg1: the landing audit re-checks what the gate refuses, from public data.
use rdy_engine::alt::AltMemberInput;
use rdy_engine::tensions::{SideRef, TensionKind, TensionSeverity, tension_label};
use rdy_engine::*;
use rdy_qa::edit_loop::{brief_of, spec_doc, stable_spec, write_cited_as};
use rdy_qa::fixtures::*;
use rdy_qa::intake::{TEST_SUMMARY, assert_not_ready, refused_ref};
use rdy_qa::landing_audit::{Landed, audit, landed_of};
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::{ag, change_of, cite, of_kind, one, put, upd};
use serde_json::json;

fn rules(v: &[rdy_qa::invariants::Violation]) -> Vec<&'static str> {
    v.iter().map(|x| x.rule).collect()
}

fn upd_at(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}

fn m(label: &str, view: &str) -> AltMemberInput {
    AltMemberInput {
        label: label.into(),
        view: view.into(),
    }
}

#[test]
fn sg1_a_clean_landing_passes_the_audit() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let w = put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "One claim."}]),
    );
    let before = r.read();
    let landed = Landed {
        view: "a".into(),
        changes: vec![w.change.parse().unwrap()],
        alternative: None,
    };
    assert!(audit(&before, &landed).is_empty());
    let l = land(&mut r, "a").unwrap();
    assert_eq!(
        landed_of(&r, &l.parse().unwrap()).unwrap().changes,
        landed.changes
    );
}

#[test]
fn sg1_the_audit_catches_a_landing_the_gate_would_refuse() {
    // two views edit one block from one base: an edit collision, blocking by default
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let base = version_of(&r, &Scope::Canon, &c);
    // both views write before either lands, from the same base
    put(
        &mut r,
        "a",
        json!([{"op": "update", "id": c, "base": base, "body": "A says ten."}]),
    );
    let b = put(
        &mut r,
        "b",
        json!([{"op": "update", "id": c, "base": base, "body": "B says five."}]),
    );
    land(&mut r, "a").unwrap();
    // a's landing rebased b onto a canon that changed the same block: the gate refuses b now;
    // the audit, given b's changes against this held state, agrees
    let before = r.read();
    let landed = Landed {
        view: "b".into(),
        changes: vec![b.change.parse().unwrap()],
        alternative: None,
    };
    assert!(
        land(&mut r, "b").is_err(),
        "precondition: the gate refuses b"
    );
    let v = audit(&before, &landed);
    // fix round 1: both rules fire here, deterministically: the blocking edit collision with
    // every side canon or b, and the conflict b's change carries onto the new canon
    let got = rules(&v);
    assert!(
        got.contains(&"blocking_landed") && got.contains(&"conflict_landed"),
        "{v:?}"
    );
    assert!(
        got.iter()
            .all(|r| *r == "blocking_landed" || *r == "conflict_landed"),
        "{v:?}"
    );
}

/// Fix round 1: a blocking tension alone (no conflict) is flagged by `blocking_landed` only; a
/// dismissed one and an advisory one are not.
#[test]
fn sg1_a_blocking_tension_alone_is_flagged_and_a_dismissed_or_advisory_one_is_not() {
    // under the QA pack an unjustified change to stable normative content is blocking
    let (_d, mut r) = qa_project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.c]).base.token;
    let w = write_cited_as(&mut r, &ag("a"), "a", upd(&s.c, "C changed."), Some(&tok)).unwrap();
    let t = one(&r, TensionKind::UnjustifiedChange);
    assert_eq!(t.severity, TensionSeverity::Blocking);
    let landed = Landed {
        view: "a".into(),
        changes: vec![change_of(&w)],
        alternative: None,
    };
    let before = r.read();
    assert!(
        before
            .conflicts(&Scope::View("a".into()))
            .unwrap()
            .is_empty(),
        "precondition: no conflict"
    );
    assert_eq!(rules(&audit(&before, &landed)), vec!["blocking_landed"]);
    // precondition: the gate refuses, with the tension as its one readiness issue (D-ME2HD)
    let e = land(&mut r, "a").unwrap_err();
    assert_not_ready(&r, &e, &refused_ref(&e), &[E_BLOCKING_TENSION]);
    // dismissed by the human: the gate lands it, and the audit does not flag it
    r.dismiss_tension(&writer(), &tension_label(&t.id), "a deliberate change")
        .unwrap();
    let before = r.read();
    assert!(
        audit(&before, &landed).is_empty(),
        "a dismissed tension never refuses"
    );
    land(&mut r, "a").unwrap();

    // the default pack: the same tension is blocking in stable content too (D-ME2HD), so the
    // audit flags it and the gate refuses
    let (_d2, mut r) = project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.c]).base.token;
    let w = write_cited_as(&mut r, &ag("a"), "a", upd(&s.c, "C changed."), Some(&tok)).unwrap();
    assert_eq!(
        one(&r, TensionKind::UnjustifiedChange).severity,
        TensionSeverity::Blocking
    );
    let landed = Landed {
        view: "a".into(),
        changes: vec![change_of(&w)],
        alternative: None,
    };
    assert_eq!(rules(&audit(&r.read(), &landed)), vec!["blocking_landed"]);
    let e = land(&mut r, "a").unwrap_err();
    assert_not_ready(&r, &e, &refused_ref(&e), &[E_BLOCKING_TENSION]);

    // task 3 fix round 1: an advisory tension (a deprecated reference stays advisory under
    // rdy-standard) with every side in canon or the landing: the audit does not flag it, and
    // the landing lands
    let (_d3, mut r) = project();
    let s = spec_doc(&mut r);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": s.section.to_string(),
        "fields": {"lifecycle": "deprecated"}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let w = put(&mut r, "b", cite(&s.section, &s.c));
    assert_eq!(
        one(&r, TensionKind::DeprecatedReference).severity,
        TensionSeverity::Advisory
    );
    let mine = change_of(&w);
    let landed = Landed {
        view: "b".into(),
        changes: vec![mine],
        alternative: None,
    };
    let before = r.read();
    let advisory_all_in = before
        .landing_tensions("b", &landed.changes)
        .unwrap()
        .into_iter()
        .filter(|t| {
            t.severity == TensionSeverity::Advisory
                && t.sides.iter().all(|s| match s {
                    SideRef::Canon { .. } => true,
                    SideRef::Change(h) => *h == mine,
                    SideRef::Intent { .. } => false,
                })
        })
        .count();
    assert!(
        advisory_all_in >= 1,
        "precondition: an advisory tension with every side in canon or the landing"
    );
    assert!(
        audit(&before, &landed).is_empty(),
        "an advisory tension never refuses"
    );
    land(&mut r, "b").unwrap();
}

/// Fix round 1 (probe P2): the audit re-checks gconf1 on the landing's own changes, not on the
/// view's current conflict set. A subset that leaves out the colliding change the view includes
/// carries no conflict: the gate lands it, and the audit does not flag it.
#[test]
fn sg1_a_subset_that_carries_no_conflict_is_not_flagged() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let sa = as_agent("agent:a", "r1");
    let ca = write_as(&mut r, &sa, "a", upd_at(&c, &v, "a's"))
        .unwrap()
        .change;
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        upd_at(&c, &v, "b's"),
    )
    .unwrap()
    .change;
    r.view_include(&sa, "a", std::slice::from_ref(&cb)).unwrap();
    let before = r.read();
    assert_eq!(
        before.conflicts(&Scope::View("a".into())).unwrap().len(),
        1,
        "precondition: view a holds the conflict"
    );
    let subset = Landed {
        view: "a".into(),
        changes: vec![ca.parse().unwrap()],
        alternative: None,
    };
    assert_eq!(
        audit(&before, &subset),
        vec![],
        "the subset carries no conflict"
    );
    // the whole set carries it
    let whole = Landed {
        changes: vec![ca.parse().unwrap(), cb.parse().unwrap()],
        ..subset.clone()
    };
    assert!(rules(&audit(&before, &whole)).contains(&"conflict_landed"));
    let p = r
        .propose_subset(
            &sa,
            "a",
            "just a",
            Some(std::slice::from_ref(&ca)),
            Some(TEST_SUMMARY),
        )
        .unwrap();
    assert!(p.blocking.is_empty());
    let l = r.land(&approver(), &p.id).unwrap();
    assert_eq!(landed_of(&r, &l.parse().unwrap()).unwrap(), subset);
}

/// Fix round 1 (probe P1b): a version that predates its view's resolution still carries the
/// conflict (gconf1 Review Focus 2). With edit_collision advisory no tension hides it: the gate
/// refuses with an `E_CONFLICT_UNRESOLVED` issue, and the audit flags `conflict_landed`.
#[test]
fn sg1_a_stale_version_over_a_resolved_view_is_flagged() {
    let (d, mut r) = project();
    std::fs::write(
        d.path().join(".rdy/packs/soft.yaml"),
        "name: soft\nversion: \"1\"\nextends: rdy-standard\ntensions:\n  edit_collision: advisory\n",
    )
    .unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: soft\n").unwrap();
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let sa = as_agent("agent:a", "r1");
    let sb = as_agent("agent:b", "r1");
    write_as(&mut r, &sa, "a", upd_at(&c, &v, "a's")).unwrap();
    let cb = write_as(&mut r, &sb, "b", upd_at(&c, &v, "b's"))
        .unwrap()
        .change;
    land(&mut r, "a").unwrap();
    let p = r.propose(&sb, "b", "b", Some(TEST_SUMMARY)).unwrap();
    assert_eq!(p.blocking.len(), 1, "the version carries the conflict");
    // b resolves the conflict in its view, after the version was proposed
    let block = rdy_model::BlockId::parse(&c).unwrap();
    let conflict = r
        .read()
        .conflict_on(&Scope::View("b".into()), &block)
        .unwrap()
        .unwrap();
    write_as(
        &mut r,
        &sb,
        "b",
        upd_at(&c, &conflict.base.unwrap().to_string(), "both"),
    )
    .unwrap();
    let before = r.read();
    assert!(
        before
            .conflicts(&Scope::View("b".into()))
            .unwrap()
            .is_empty(),
        "precondition: the view is resolved"
    );
    assert!(
        of_kind(&r, TensionKind::EditCollision)
            .iter()
            .all(|t| t.severity == TensionSeverity::Advisory),
        "precondition: no blocking edit collision hides the conflict"
    );
    let stale = Landed {
        view: "b".into(),
        changes: vec![cb.parse().unwrap()],
        alternative: None,
    };
    let v = audit(&before, &stale);
    assert_eq!(rules(&v), vec!["conflict_landed"], "{v:?}");
    assert_eq!(v[0].block.as_ref(), Some(&block));
    assert_eq!(
        v[0].detail,
        format!("landing of b: block {c} had an unresolved conflict")
    );
    // precondition: the gate refuses, with the conflict as its one readiness issue (D-ME2HD)
    let e = r.land_report(&approver(), &p.r#ref).unwrap_err();
    assert_not_ready(&r, &e, &p.r#ref, &[E_CONFLICT_UNRESOLVED]);
}

#[test]
fn sg1_an_unchosen_alternative_is_flagged() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let x = put(
        &mut r,
        "x",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "X."}]),
    );
    put(
        &mut r,
        "y",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "Y."}]),
    );
    let set = r
        .alt_create(&ag("x"), "Which", None, &[m("x", "x"), m("y", "y")])
        .unwrap();
    // written into a member view after the set exists: the change links to the set
    let w = put(
        &mut r,
        "x",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "X two."}]),
    );
    let before = r.read();
    let landed = Landed {
        view: "x".into(),
        changes: vec![x.change.parse().unwrap(), w.change.parse().unwrap()],
        alternative: None,
    };
    assert!(rules(&audit(&before, &landed)).contains(&"alt_unchosen"));
    let ok = Landed {
        alternative: Some(set.set.parse().unwrap()),
        ..landed
    };
    assert!(!rules(&audit(&before, &ok)).contains(&"alt_unchosen"));
}

/// Members x (view x) and y (view y) of one set; each member writes a claim after the set exists
/// (so both changes link to it). x proposes choosing x, writes `later` (still linked: the set is
/// open), and the proposal lands. Returns the changes of `later` and of y's second claim.
fn chosen_x_then_more(r: &mut Rdy) -> (rdy_model::Hash, rdy_model::Hash) {
    let (_doc, section, _c) = seed_doc(r, "seed");
    land(r, "seed").unwrap();
    let claim =
        |body: &str| json!([{"op": "create", "type": "claim", "parent": section, "body": body}]);
    put(r, "x", claim("X."));
    put(r, "y", claim("Y."));
    let set = r
        .alt_create(&ag("x"), "Which", None, &[m("x", "x"), m("y", "y")])
        .unwrap();
    put(r, "x", claim("X two."));
    let lost = put(r, "y", claim("Y two."));
    let pick = rdy_engine::alt::AltPick {
        set: set.id.clone(),
        chosen: "x".into(),
    };
    let p = r
        .propose_with(&writer(), "x", "x", None, Some(TEST_SUMMARY), Some(&pick))
        .unwrap();
    let later = put(r, "x", claim("X three."));
    r.land(&approver(), &p.id).unwrap();
    (change_of(&later), change_of(&lost))
}

/// Ruling task-5.1 (seed 3's pattern; gate.rs linked_alts rule 2): once a set is chosen, the
/// chosen member's later changes land without choosing again, and the audit agrees.
#[test]
fn sg1_the_chosen_members_later_change_lands_without_a_choice() {
    let (_d, mut r) = project();
    let (later, _lost) = chosen_x_then_more(&mut r);
    let before = r.read();
    let landed = Landed {
        view: "x".into(),
        changes: vec![later],
        alternative: None,
    };
    let v = audit(&before, &landed);
    assert!(!rules(&v).contains(&"alt_unchosen"), "{v:?}");
    drop(before);
    // precondition: the gate lands it, choosing nothing
    let l = land(&mut r, "x").unwrap();
    let rec = landed_of(&r, &l.parse().unwrap()).unwrap();
    assert_eq!((rec.changes, rec.alternative), (vec![later], None));
}

/// Ruling task-5.1 (guard): a change bound to a member the set did not choose, landed with no
/// choice, is still flagged.
#[test]
fn sg1_a_losing_members_change_landed_without_a_choice_is_flagged() {
    let (_d, mut r) = project();
    let (_later, lost) = chosen_x_then_more(&mut r);
    let before = r.read();
    let landed = Landed {
        view: "z".into(),
        changes: vec![lost],
        alternative: None,
    };
    assert!(rules(&audit(&before, &landed)).contains(&"alt_unchosen"));
}

#[test]
fn sg1_check_all_covers_canon_and_every_open_view() {
    // guard: check_all is check on canon plus check on each open view plus the logged rule
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "A."}]),
    );
    rdy_qa::invariants::check_all(&r).unwrap();
}
