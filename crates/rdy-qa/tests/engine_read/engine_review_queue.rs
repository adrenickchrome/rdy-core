//! se6: the review queue and its case files: order, skips, leases, a stable case hash.
use rdy_engine::semantic::api::{ReviewNextRequest, run_review_next};
use rdy_engine::semantic::case::*;
use rdy_engine::{Engine, Local, Session, pin_clock};
use rdy_model::Role;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use std::collections::BTreeSet;

fn reviewer(actor: &str) -> Session {
    Session::agent(actor.into(), Some("r".into()), Role::Reviewer).unwrap()
}
fn next(e: &Local, s: &Session, owner: u64) -> serde_json::Value {
    run_review_next(
        e,
        s,
        owner,
        ReviewNextRequest {
            kind: "tension".into(),
        },
    )
    .unwrap()
}

#[test]
fn se6_the_queue_runs_oldest_change_first() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // the pinned clock moves a millisecond per read, so c1 is older than c2 on every run
    let _c = pin_clock(1_000);
    let c1 = change_of(&put(&mut r, "a", upd(&s.c, "C changed.")));
    let c2 = change_of(&put(&mut r, "b", upd(&s.a, "A changed.")));
    let q = r.read().review_requests().unwrap();
    let changes: Vec<_> = q.iter().map(|x| x.change).collect();
    assert_eq!(changes.first(), Some(&c1));
    assert!(changes.contains(&c2));
    let mut keys: Vec<_> = q.iter().map(|x| x.key).collect();
    let n = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), n, "a pair once");
}

#[test]
fn se6_next_skips_leased_and_own_requests() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C changed."));
    let st = r.read();
    let all = st.review_requests().unwrap();
    assert_eq!(all.len(), 3, "C pairs with B, D and A");
    assert_eq!(
        st.next_request("agent:a", &BTreeSet::new()).unwrap(),
        None,
        "agent:a wrote C"
    );
    let first = st
        .next_request("agent:r", &BTreeSet::new())
        .unwrap()
        .unwrap();
    assert_eq!(first, all[0]);
    let leased: BTreeSet<_> = [first.key].into();
    assert_eq!(
        st.next_request("agent:r", &leased).unwrap().unwrap(),
        all[1]
    );
}

#[test]
fn se6_the_case_and_its_hash_are_a_function_of_the_state() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C changed."));
    let st = r.read();
    // the (A, C) request: A is decided_by D
    let q = st
        .review_requests()
        .unwrap()
        .into_iter()
        .find(|q| q.items.iter().any(|i| i.block == s.a))
        .unwrap();
    let c1 = st.case_of(&q.key, &q.items).unwrap();
    let c2 = st.case_of(&q.key, &q.items).unwrap();
    assert_eq!(case_hash(&c1), case_hash(&c2));
    assert_eq!(c1.kind, "tension");
    assert_eq!(c1.question, CASE_QUESTION);
    assert!(c1.schema["severity"].is_string());
    assert_eq!(c1.blocks.len(), 2);
    assert_eq!(
        c1.blocks[0].headings,
        vec!["Spec".to_string(), "Rules".to_string()]
    );
    let a = c1.blocks.iter().find(|b| b.id == s.a.to_string()).unwrap();
    assert_eq!(
        a.governing[0].id,
        s.decision.to_string(),
        "A is decided_by D"
    );
    drop(st);
    put(&mut r, "a", upd(&s.c, "C changed again."));
    let st = r.read();
    let keys: Vec<_> = st
        .review_requests()
        .unwrap()
        .iter()
        .map(|x| x.key)
        .collect();
    assert!(!keys.contains(&q.key), "a content change makes a new pair");
}

#[test]
fn se6_next_leases_the_request_and_a_second_reviewer_gets_the_next_one() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C changed."));
    let _clock = pin_clock(1_000);
    let e = Local::new(r);
    let one = next(&e, &reviewer("agent:r1"), 1);
    let two = next(&e, &reviewer("agent:r2"), 2);
    let three = next(&e, &reviewer("agent:r3"), 3);
    assert_ne!(one["request"], two["request"]);
    assert!(
        three["request"].is_string()
            && three["request"] != one["request"]
            && three["request"] != two["request"]
    );
    assert_eq!(one["case_hash"].as_str().unwrap().len(), 64);
    assert!(one["lease_expires_at"].is_string());
    let none = next(&e, &reviewer("agent:r4"), 4);
    assert_eq!(none["request"], serde_json::Value::Null);
    assert_eq!(none["leased"], 3);
    e.leases().release_owner(1);
    assert_eq!(
        next(&e, &reviewer("agent:r4"), 4)["request"],
        one["request"]
    );
}

#[test]
fn se6_a_lease_lapses_after_the_ttl() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C changed."));
    let e = Local::new(r);
    let first = {
        let _c = pin_clock(0);
        next(&e, &reviewer("agent:r1"), 1)
    };
    // the pinned clock moves a millisecond per read: leave a margin past 10 minutes
    let _c = pin_clock(600_100);
    let again = next(&e, &reviewer("agent:r2"), 2);
    assert_eq!(
        again["request"], first["request"],
        "the 10-minute lease lapsed"
    );
}

#[test]
fn se6_next_needs_the_reviewer_role_and_kind_tension() {
    let (_d, r) = project();
    let e = Local::new(r);
    let w = run_review_next(
        &e,
        &ag("w"),
        1,
        ReviewNextRequest {
            kind: "tension".into(),
        },
    )
    .unwrap_err();
    assert_eq!(
        (w.code, w.message.as_str()),
        (
            rdy_engine::E_PERMISSION,
            "rdy_review_next needs the reviewer role"
        )
    );
    let k = run_review_next(
        &e,
        &reviewer("agent:r"),
        1,
        ReviewNextRequest {
            kind: "check".into(),
        },
    )
    .unwrap_err();
    assert_eq!(
        k.message,
        "kind \"check\" is not a review kind; the only kind is tension"
    );
}

#[test]
fn se6_a_repin_keeps_the_text_writer_as_an_author() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // agent:w writes X (it cites A) and X lands
    let x = created(&put(&mut r, "w", cite(&s.section, &s.a)), "$y");
    land(&mut r, "w").unwrap();
    // A changes and lands; agent:p only repins X, so X's text stays agent:w's
    put(&mut r, "q", upd(&s.a, "A changed."));
    land(&mut r, "q").unwrap();
    let cp = change_of(&put(
        &mut r,
        "p",
        serde_json::json!([{"op": "repin", "id": x.to_string()}]),
    ));
    let st = r.read();
    let all = st.review_requests().unwrap();
    assert!(!all.is_empty(), "the repin makes requests");
    for q in &all {
        let i = q
            .items
            .iter()
            .find(|i| i.block == x)
            .expect("every pair holds X");
        assert_eq!(i.side, rdy_engine::tensions::SideRef::Change(cp));
        let authors = st.item_authors_of(&q.key, i).unwrap();
        assert!(authors.contains(&"agent:p".to_string()), "{authors:?}");
        assert!(authors.contains(&"agent:w".to_string()), "{authors:?}");
    }
    assert_eq!(
        st.next_request("agent:w", &BTreeSet::new()).unwrap(),
        None,
        "agent:w wrote X's text"
    );
    assert_eq!(st.next_request("agent:p", &BTreeSet::new()).unwrap(), None);
    assert!(
        st.next_request("agent:r", &BTreeSet::new())
            .unwrap()
            .is_some()
    );
}

#[test]
fn se6_identical_text_by_a_second_actor_adds_both_authors() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "u", upd(&s.c, "C changed."));
    put(&mut r, "v", upd(&s.c, "C changed."));
    let st = r.read();
    let all = st.review_requests().unwrap();
    assert!(!all.is_empty());
    for q in &all {
        let i = q
            .items
            .iter()
            .find(|i| i.block == s.c)
            .expect("every pair holds C");
        let authors = st.item_authors_of(&q.key, i).unwrap();
        assert!(authors.contains(&"agent:u".to_string()), "{authors:?}");
        assert!(authors.contains(&"agent:v".to_string()), "{authors:?}");
    }
    for a in ["agent:u", "agent:v"] {
        assert_eq!(
            st.next_request(a, &BTreeSet::new()).unwrap(),
            None,
            "{a} wrote C's text"
        );
    }
    assert!(
        st.next_request("agent:r", &BTreeSet::new())
            .unwrap()
            .is_some()
    );
}

#[test]
fn se6_a_landed_repin_keeps_the_text_writer_as_a_canon_author() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // agent:w writes X (it cites A) and X lands; A changes; agent:p's repin of X lands
    let x = created(&put(&mut r, "w", cite(&s.section, &s.a)), "$y");
    land(&mut r, "w").unwrap();
    put(&mut r, "q", upd(&s.a, "A changed."));
    land(&mut r, "q").unwrap();
    put(
        &mut r,
        "p",
        serde_json::json!([{"op": "repin", "id": x.to_string()}]),
    );
    land(&mut r, "p").unwrap();
    // A changes again: its pairs with X hold X's canon item, which agent:p's landing carried
    put(&mut r, "t", upd(&s.a, "A changed again."));
    let st = r.read();
    let with_x: Vec<_> = st
        .review_requests()
        .unwrap()
        .into_iter()
        .filter(|q| q.items.iter().any(|i| i.block == x))
        .collect();
    assert!(!with_x.is_empty(), "A pairs with X");
    for q in &with_x {
        let i = q.items.iter().find(|i| i.block == x).unwrap();
        assert!(matches!(
            i.side,
            rdy_engine::tensions::SideRef::Canon { .. }
        ));
        let authors = st.item_authors_of(&q.key, i).unwrap();
        assert!(authors.contains(&"agent:p".to_string()), "{authors:?}");
        assert!(authors.contains(&"agent:w".to_string()), "{authors:?}");
    }
    // walk every request agent:w is offered: none holds X
    let mut leased = BTreeSet::new();
    while let Some(q) = st.next_request("agent:w", &leased).unwrap() {
        assert!(
            q.items.iter().all(|i| i.block != x),
            "agent:w wrote X's text"
        );
        leased.insert(q.key);
    }
}
