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

fn ids(c: &str, t: &str) -> (rdy_model::BlockId, rdy_model::BlockId) {
    (
        rdy_model::BlockId::parse(c).unwrap(),
        rdy_model::BlockId::parse(t).unwrap(),
    )
}

#[test]
fn cc4_a_restore_holds_across_a_reopen_and_a_later_dismissal_too() {
    let (d, mut r, c, t) = world();
    let (cb, tb) = ids(&c, &t);
    r.review_mention(&writer(), &w(), &c, &t, MentionVerdict::Dismiss, "passing")
        .unwrap();
    r.review_mention(
        &writer(),
        &w(),
        &c,
        &t,
        MentionVerdict::Restore,
        "it is the service",
    )
    .unwrap();
    drop(r);
    // the full publish at open keeps only the pairs whose latest review dismisses
    let mut r = Rdy::open(d.path()).unwrap();
    assert!(
        r.read().mention_dismissed(&cb, &tb).is_none(),
        "a restored mention stays open after a reopen"
    );
    r.review_mention(
        &writer(),
        &w(),
        &c,
        &t,
        MentionVerdict::Dismiss,
        "passing again",
    )
    .unwrap();
    drop(r);
    let r = Rdy::open(d.path()).unwrap();
    assert!(
        r.read().mention_dismissed(&cb, &tb).is_some(),
        "a later dismissal wins after a reopen"
    );
}

#[test]
fn cc4_a_linked_mention_is_refused_and_a_bad_id_is_bad_input() {
    let (_d, mut r) = project();
    let wr = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$o", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$o", "body": "Every write passes the Ledger ({{$t}})."}
    ])).unwrap();
    let (c, t) = (wr.created["$c"].clone(), wr.created["$t"].clone());
    let e = r
        .review_mention(&writer(), &w(), &c, &t, MentionVerdict::Dismiss, "x")
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.contains("does not name"), "{}", e.message);
    assert!(
        e.fix.contains("a linked mention needs no review"),
        "{}",
        e.fix
    );
    let (cb, tb) = ids(&c, &t);
    assert!(r.read().mention_dismissed(&cb, &tb).is_none());
    for (b, en) in [("not-an-id", t.as_str()), (c.as_str(), "not-an-id")] {
        let e = r
            .review_mention(&writer(), &w(), b, en, MentionVerdict::Dismiss, "x")
            .unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT);
        assert!(e.fix.contains("pass the block id"), "{}", e.fix);
    }
}

#[test]
fn cc4_a_blank_or_overlong_reason_says_what_a_reason_is() {
    let (_d, mut r, c, t) = world();
    for reason in ["  ".to_string(), "x".repeat(2001)] {
        let e = r
            .review_mention(&writer(), &w(), &c, &t, MentionVerdict::Dismiss, &reason)
            .unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT);
        assert_eq!(
            e.message,
            "a mention review needs a reason of 1 to 2000 characters"
        );
        assert_eq!(
            e.fix,
            "say in a few words why the mention is passing, or why it needs a link again"
        );
    }
    // the largest allowed reason passes
    r.review_mention(
        &writer(),
        &w(),
        &c,
        &t,
        MentionVerdict::Dismiss,
        &"x".repeat(2000),
    )
    .unwrap();
}
