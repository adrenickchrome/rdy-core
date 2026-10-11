//! cc4: reviewing a passing mention (FW-CV4): dismiss and restore, recorded with the actor, the
//! latest review per (block, entity) wins, and the state carries the dismissed pairs.
use rdy_engine::mention_review::MentionVerdict;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

fn w() -> Scope {
    Scope::View("work".into())
}

/// A term "Ledger" and a claim that names it without a link, in view "work".
fn world() -> (tempfile::TempDir, Rdy, String, String) {
    let (d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$o", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$o", "body": "Every write passes the Ledger."}
    ])).unwrap();
    (d, r, w.created["$c"].clone(), w.created["$t"].clone())
}

#[test]
fn cc4_a_writer_dismisses_and_the_state_carries_it() {
    let (_d, mut r, c, t) = world();
    let i = r
        .review_mention(
            &writer(),
            &w(),
            &c,
            &t,
            MentionVerdict::Dismiss,
            "everyday word here",
        )
        .unwrap();
    assert_eq!(i.status, "dismissed");
    let (cb, tb) = (
        rdy_model::BlockId::parse(&c).unwrap(),
        rdy_model::BlockId::parse(&t).unwrap(),
    );
    let h = r.read().mention_dismissed(&cb, &tb).expect("dismissed");
    assert_eq!(
        r.read().reader().review(&h).unwrap().provenance.actor,
        "human:qa"
    );
}

#[test]
fn cc4_the_latest_review_per_mention_wins() {
    let (_d, mut r, c, t) = world();
    r.review_mention(&writer(), &w(), &c, &t, MentionVerdict::Dismiss, "passing")
        .unwrap();
    let i = r
        .review_mention(
            &writer(),
            &w(),
            &c,
            &t,
            MentionVerdict::Restore,
            "it is the ledger service",
        )
        .unwrap();
    assert_eq!(i.status, "open");
    let (cb, tb) = (
        rdy_model::BlockId::parse(&c).unwrap(),
        rdy_model::BlockId::parse(&t).unwrap(),
    );
    assert!(r.read().mention_dismissed(&cb, &tb).is_none());
    r.review_mention(
        &writer(),
        &w(),
        &c,
        &t,
        MentionVerdict::Dismiss,
        "passing after all",
    )
    .unwrap();
    assert!(r.read().mention_dismissed(&cb, &tb).is_some());
}

#[test]
fn cc4_a_dismissal_settles_the_pair_in_every_scope_and_survives_reopen() {
    let (d, mut r, c, t) = world();
    r.review_mention(&writer(), &w(), &c, &t, MentionVerdict::Dismiss, "passing")
        .unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    let r = Rdy::open(d.path()).unwrap();
    let (cb, tb) = (
        rdy_model::BlockId::parse(&c).unwrap(),
        rdy_model::BlockId::parse(&t).unwrap(),
    );
    assert!(
        r.read().mention_dismissed(&cb, &tb).is_some(),
        "a full publish re-reads the refs"
    );
}

#[test]
fn cc4_refusals_name_their_fix() {
    let (_d, mut r, c, t) = world();
    let e = r
        .review_mention(&reader(), &w(), &c, &t, MentionVerdict::Dismiss, "x")
        .unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
    let e = r
        .review_mention(&writer(), &w(), &c, &t, MentionVerdict::Dismiss, "  ")
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    let e = r
        .review_mention(&writer(), &w(), &t, &c, MentionVerdict::Dismiss, "x")
        .unwrap_err();
    assert!(e.message.contains("does not name"), "{}", e.message);
    let e = r
        .review_mention(&writer(), &w(), &c, &t, MentionVerdict::Restore, "x")
        .unwrap_err();
    assert!(e.message.contains("is not dismissed"), "{}", e.message);
    r.review_mention(&writer(), &w(), &c, &t, MentionVerdict::Dismiss, "x")
        .unwrap();
    let e = r
        .review_mention(&writer(), &w(), &c, &t, MentionVerdict::Dismiss, "x")
        .unwrap_err();
    assert!(e.message.contains("already dismissed"), "{}", e.message);
}
