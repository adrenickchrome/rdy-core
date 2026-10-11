//! se3: the semantic state of a published state: verdicts and semantic tensions read at open,
//! after reindex, and after each recorded verdict or version.
use rdy_engine::{Rdy, Scope};
use rdy_model::*;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;

fn content(r: &Rdy, id: &BlockId) -> Hash {
    let m = r.read().materialized(&Scope::Canon).unwrap();
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

#[test]
fn se3_a_recorded_verdict_is_in_the_next_state_and_after_reopen_and_reindex() {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    let items = pair(&r, &s.a, &s.c);
    let key = pair_key(&items);
    let rv = verdict(items.clone(), Verdict::Tension);
    let (h, t) = r.record_verdict(&rv, Some(&first(&rv, items))).unwrap();
    let st = r.read();
    assert_eq!(st.semantic().settled(&key).unwrap().review, h);
    assert_eq!(
        st.semantic().settled(&key).unwrap().verdict,
        Verdict::Tension
    );
    assert_eq!(st.semantic().tension(&key).unwrap().version, t.unwrap());
    assert_eq!(st.semantic().involving(&s.a).len(), 1);
    assert!(st.semantic().involving(&s.b).is_empty());
    drop(st);
    r.close().unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    assert!(r.read().semantic().tension(&key).is_some());
    r.reindex().unwrap();
    assert!(r.read().semantic().tension(&key).is_some());
}

#[test]
fn se3_a_no_tension_verdict_settles_without_a_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let items = pair(&r, &s.b, &s.c);
    r.record_verdict(&verdict(items.clone(), Verdict::NoTension), None)
        .unwrap();
    let st = r.read();
    assert_eq!(
        st.semantic().settled(&pair_key(&items)).unwrap().verdict,
        Verdict::NoTension
    );
    assert!(!st.semantic().has_tensions());
}

#[test]
fn se3_an_acceptance_replaces_the_open_record() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let items = pair(&r, &s.a, &s.c);
    let rv = verdict(items.clone(), Verdict::Tension);
    let (_, t) = r
        .record_verdict(&rv, Some(&first(&rv, items.clone())))
        .unwrap();
    let mut next = first(&rv, items.clone());
    next.prev = t;
    next.blocking = true;
    next.relay = Some(RelayEvidence {
        quoted: "accept it".into(),
        session_ref: "chat".into(),
        question: None,
    });
    let t2 = r.record_tension_versions(&[next]).unwrap();
    let st = r.read();
    let open = st.semantic().tension(&pair_key(&items)).unwrap();
    assert_eq!((open.version, open.record.blocking), (t2[0], true));
}

#[test]
fn se3_a_refused_record_is_e_review_stale_and_publishes_nothing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let items = pair(&r, &s.a, &s.c);
    r.record_verdict(&verdict(items.clone(), Verdict::NoTension), None)
        .unwrap();
    let seq = r.read().seq();
    let mut again = verdict(items, Verdict::NoTension);
    again.provenance.at_ms = 2;
    assert_eq!(
        r.record_verdict(&again, None).unwrap_err().code,
        rdy_engine::E_REVIEW_STALE
    );
    assert_eq!(r.read().seq(), seq);
}

#[test]
fn se3_the_lease_ttl_comes_from_the_project_config() {
    let (d, r) = project();
    assert_eq!(r.read().lease_ttl_ms(), 600_000);
    r.close().unwrap();
    let cfg = d.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, format!("{text}reviews:\n  lease_ttl: 2\n")).unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.read().lease_ttl_ms(), 120_000);
}

/// guard: a verdict keeps every built view (only the semantic state and the tensions move).
#[test]
fn se3_a_verdict_carries_the_built_views() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.read().materialized(&Scope::Canon).unwrap();
    let before = r.read().materialized(&Scope::Canon).unwrap();
    let items = pair(&r, &s.a, &s.c);
    r.record_verdict(&verdict(items, Verdict::NoTension), None)
        .unwrap();
    let after = r.read().materialized(&Scope::Canon).unwrap();
    assert!(std::sync::Arc::ptr_eq(&before, &after));
}
