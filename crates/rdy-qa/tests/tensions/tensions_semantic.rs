//! se5: the semantic detector: a recorded tension verdict is a tension with the usual lifecycle.
use rdy_engine::tensions::set::full_builds;
use rdy_engine::tensions::*;
use rdy_engine::{Rdy, Scope};
use rdy_model::*;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;

/// `id`'s content in flight (canon's when no in-flight change writes it).
fn content(r: &Rdy, id: &BlockId) -> Hash {
    let m = r.read().materialized(&Scope::Inflight).unwrap();
    m.get(id).unwrap().content_hash_with(r.store().tracking())
}
fn pair(r: &Rdy, a: &BlockId, b: &BlockId) -> Vec<PairItem> {
    let mut v = vec![
        PairItem {
            block: a.clone(),
            content: content(r, a),
        },
        PairItem {
            block: b.clone(),
            content: content(r, b),
        },
    ];
    v.sort();
    v
}
fn verdict(items: Vec<PairItem>, v: Verdict) -> Review {
    Review {
        subject: ReviewSubject::Pair {
            key: pair_key(&items),
            items,
        },
        case_hash: Some(Hash([9; 32])),
        verdict: v,
        reason: String::new(),
        findings: vec![],
        provenance: Provenance {
            actor: "agent:r".into(),
            run: Some("r".into()),
            role: Role::Reviewer,
            at_ms: 1,
        },
    }
}
/// The first version of the pair's semantic tension, with the reviewer's advice to block.
fn first(r: &Review, items: Vec<PairItem>) -> SemanticTension {
    SemanticTension {
        key: pair_key(&items),
        items,
        review: Hash::of(&postcard::to_allocvec(r).unwrap()),
        advice: Some("it contradicts the gate rule".into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: r.provenance.clone(),
    }
}

fn judge(r: &mut Rdy, a: &BlockId, b: &BlockId) -> Hash {
    let items = pair(r, a, b);
    let rv = verdict(items.clone(), Verdict::Tension);
    r.record_verdict(&rv, Some(&first(&rv, items.clone())))
        .unwrap();
    pair_key(&items)
}

#[test]
fn se5_a_tension_verdict_opens_an_advisory_semantic_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts A."));
    judge(&mut r, &s.a, &s.c);
    let t = one(&r, TensionKind::Semantic);
    assert_eq!(t.severity, TensionSeverity::Advisory);
    assert!(has_canon(&t) && change_sides(&t).len() == 1);
    let (lo, hi) = if s.a < s.c {
        (&s.a, &s.c)
    } else {
        (&s.c, &s.a)
    };
    assert_eq!(
        t.summary,
        format!("a reviewer judged that {lo} and {hi} disagree")
    );
}

#[test]
fn se5_a_content_change_resolves_a_repin_keeps_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(
        &mut r,
        "a",
        upd(&s.b, &format!("B builds on {{{{{}}}}} and disagrees.", s.a)),
    );
    // A moves on, so B's pin is behind and a repin has something to do
    put(&mut r, "a", upd(&s.a, "A holds firmly."));
    judge(&mut r, &s.a, &s.b);
    assert_eq!(of_kind(&r, TensionKind::Semantic).len(), 1);
    put(
        &mut r,
        "a",
        serde_json::json!([{"op": "repin", "id": s.b.to_string(), "targets": [s.a.to_string()]}]),
    );
    assert_eq!(
        of_kind(&r, TensionKind::Semantic).len(),
        1,
        "a repin keeps the content"
    );
    put(&mut r, "a", upd(&s.b, "B agrees now."));
    assert!(
        of_kind(&r, TensionKind::Semantic).is_empty(),
        "a content change resolves it"
    );
}

#[test]
fn se5_landing_a_side_rekeys_and_the_tension_stays_in_canon() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts A."));
    judge(&mut r, &s.a, &s.c);
    let before = one(&r, TensionKind::Semantic);
    land(&mut r, "a").unwrap();
    let after = one(&r, TensionKind::Semantic);
    assert_ne!(after.id, before.id);
    assert_eq!(after.vkey, before.vkey, "a re-key, not a new judgment");
    assert!(change_sides(&after).is_empty(), "every side is canon (F6)");
    assert!(r.read().tensions().unwrap().from_canon(&after.id));
}

#[test]
fn se5_advice_alone_keeps_it_advisory_and_an_accepted_version_is_blocking() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts A."));
    let key = judge(&mut r, &s.a, &s.c); // first() gives advice
    let before = one(&r, TensionKind::Semantic);
    assert_eq!(
        before.severity,
        TensionSeverity::Advisory,
        "advice gates nothing"
    );
    let open = r.read().semantic().tension(&key).unwrap().clone();
    let mut next = open.record.clone();
    next.prev = Some(open.version);
    next.blocking = true;
    next.relay = Some(RelayEvidence {
        quoted: "accept the advice".into(),
        session_ref: "chat".into(),
        question: None,
    });
    r.record_tension_versions(&[next]).unwrap();
    let t = one(&r, TensionKind::Semantic);
    assert_eq!(t.severity, TensionSeverity::Blocking);
    assert!(!t.overridden);
    assert_ne!(t.id, before.id, "the id hashes the severity");
}

#[test]
fn se5_a_verdict_carries_the_set_and_matches_a_full_build() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts A."));
    r.read().tensions().unwrap();
    let builds = full_builds();
    judge(&mut r, &s.a, &s.c);
    let carried: Vec<_> = open(&r).iter().map(|t| t.id).collect();
    assert_eq!(full_builds(), builds, "the verdict carried the set");
    r.invalidate();
    let full: Vec<_> = open(&r).iter().map(|t| t.id).collect();
    assert_eq!(carried, full);
}

#[test]
fn se5_pair_of_reads_the_key_from_a_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts A."));
    let key = judge(&mut r, &s.a, &s.c);
    assert_eq!(
        rdy_engine::tensions::semantic::pair_of(&one(&r, TensionKind::Semantic)),
        Some(key)
    );
}

/// guard: an acceptance on an all-canon tension carries the canon part too (F6): the blocking
/// form replaces the advisory one in canon, with no full build, as a full build gives it.
#[test]
fn se5_an_acceptance_in_canon_carries_the_canon_part() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts A."));
    let key = judge(&mut r, &s.a, &s.c);
    land(&mut r, "a").unwrap();
    let before = one(&r, TensionKind::Semantic);
    let builds = full_builds();
    let open_t = r.read().semantic().tension(&key).unwrap().clone();
    let mut next = open_t.record.clone();
    next.prev = Some(open_t.version);
    next.blocking = true;
    next.relay = Some(RelayEvidence {
        quoted: "accept the advice".into(),
        session_ref: "chat".into(),
        question: None,
    });
    r.record_tension_versions(&[next]).unwrap();
    let t = one(&r, TensionKind::Semantic);
    assert_eq!(full_builds(), builds, "the acceptance carried the set");
    assert_eq!(t.severity, TensionSeverity::Blocking);
    assert_ne!(t.id, before.id);
    assert!(r.read().tensions().unwrap().from_canon(&t.id));
    let carried: Vec<_> = open(&r).iter().map(|t| t.id).collect();
    r.invalidate();
    let full: Vec<_> = open(&r).iter().map(|t| t.id).collect();
    assert_eq!(carried, full);
}

/// A person's acceptance of the advice on the pair of key `key`: its next version blocks.
fn accept(r: &mut Rdy, key: &Hash) {
    let open_t = r.read().semantic().tension(key).unwrap().clone();
    let mut next = open_t.record.clone();
    next.prev = Some(open_t.version);
    next.blocking = true;
    next.relay = Some(RelayEvidence {
        quoted: "accept the advice".into(),
        session_ref: "chat".into(),
        question: None,
    });
    r.record_tension_versions(&[next]).unwrap();
}

/// fix round 1: `pair_of` names only a semantic tension. Another kind's tension on a judged
/// pair (here a premise shift) has no pair key, so the semantic code never takes it for the
/// pair's semantic tension.
#[test]
fn se5_pair_of_names_only_a_semantic_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let y = created(&put(&mut r, "b", cite(&s.section, &s.a)), "$y");
    put(&mut r, "a", upd(&s.a, "A changed."));
    let key = judge(&mut r, &s.a, &y);
    let shift = one(&r, TensionKind::PremiseShift);
    assert_eq!(rdy_engine::tensions::semantic::pair_of(&shift), None);
    assert_eq!(
        rdy_engine::tensions::semantic::pair_of(&one(&r, TensionKind::Semantic)),
        Some(key)
    );
}

/// guard (fix round 1): records on contents a block no longer holds give no tension, and the
/// detector's per-run memo of each block's sides gives a carry that matches a full build.
#[test]
fn se5_records_on_old_contents_give_nothing_and_a_carry_matches_a_full_build() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    for i in 0..3 {
        put(
            &mut r,
            "a",
            upd(&s.c, &format!("C contradicts A, take {i}.")),
        );
        judge(&mut r, &s.a, &s.c);
        judge(&mut r, &s.b, &s.c);
    }
    let now: Vec<_> = of_kind(&r, TensionKind::Semantic);
    assert_eq!(
        now.len(),
        2,
        "only the records on C's current content: {now:?}"
    );
    put(&mut r, "b", upd(&s.b, "B moves on."));
    put(&mut r, "a", upd(&s.c, "C contradicts A, take 2."));
    let carried: Vec<_> = open(&r).iter().map(|t| t.id).collect();
    assert_eq!(of_kind(&r, TensionKind::Semantic).len(), 1, "B moved on");
    r.invalidate();
    let full: Vec<_> = open(&r).iter().map(|t| t.id).collect();
    assert_eq!(carried, full);
    land(&mut r, "a").unwrap();
    let carried: Vec<_> = open(&r).iter().map(|t| t.id).collect();
    r.invalidate();
    let full: Vec<_> = open(&r).iter().map(|t| t.id).collect();
    assert_eq!(carried, full);
}

/// fix round 1 (FW-LP6b): a refusal never names an action that does not help. A repin keeps
/// a semantic tension, so the refusal for an accepted one names a text change, not a repin.
#[test]
fn se5_a_refusal_for_an_accepted_semantic_tension_names_a_text_change_not_a_repin() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts A."));
    let key = judge(&mut r, &s.a, &s.c);
    accept(&mut r, &key);
    let e = land(&mut r, "a").unwrap_err();
    assert_eq!(e.code, "E_BLOCKING_TENSION");
    assert!(
        e.fix
            .starts_with("change the text of a side, or dismiss it ("),
        "{}",
        e.fix
    );
    assert!(!e.fix.contains("repin"), "{}", e.fix);
    // the issue names the landing form, whose id differs from the in-flight one
    assert_eq!(e.issues.len(), 1);
    let fix = &e.issues[0].fix;
    assert!(
        fix.starts_with("tn-") && fix.ends_with(": change the text of a side, or dismiss it"),
        "{fix}"
    );
}

/// guard (fix round 1): an existing kind's refusal keeps its text.
#[test]
fn se5_an_existing_kinds_refusal_text_is_unchanged() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "b", cite(&s.section, &s.a));
    put(&mut r, "a", upd(&s.a, "A changed."));
    land(&mut r, "a").unwrap();
    let e = land(&mut r, "b").unwrap_err();
    assert_eq!(e.code, "E_BLOCKING_TENSION");
    assert_eq!(
        e.fix,
        "resolve each with a change, repin the citation, or dismiss it (rdy resolve <tn-…> --dismiss <reason>; a reviewer of another actor or a human); rdy tensions --involving <change> lists them, a form only this landing gives with its proposal"
    );
    assert_eq!(e.issues.len(), 1);
    let fix = &e.issues[0].fix;
    assert!(fix.starts_with("resolve, repin or dismiss tn-"), "{fix}");
}
