//! di11 (D-JV6Z8, O10): the inbox entry `pending_added` for a new proposal version and for a
//! verdict that carries advice. It never reads readiness, so a re-derived inbox is the same.
use rdy_engine::Rdy;
use rdy_engine::inbox::api::INBOX_DOC;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::intake::verdict_on;
use rdy_qa::reindex_check::{derived, reindex_reproduces};
use rdy_qa::tensions::{ag, created, put};
use rdy_store::inbox::{InboxCursor, InboxSub};
use serde_json::json;

const WATCHER: &str = "agent:watcher";

/// A subscription of `agent:watcher` that starts at the head of the log.
fn watcher_at_head(r: &Rdy) -> InboxSub {
    InboxSub {
        actor: WATCHER.into(),
        run: None,
        cursor: InboxCursor {
            at: r.read().seq(),
            skip: 0,
        },
        areas: vec![],
        role: "reader".into(),
    }
}

/// The `pending_added` entries `sub` has from its cursor to the end of the log: (ids, line).
fn pending_added(r: &Rdy, sub: &InboxSub) -> Vec<(Vec<String>, String)> {
    r.read()
        .inbox_page(sub, None, usize::MAX)
        .expect("inbox page")
        .entries
        .into_iter()
        .filter(|e| e.event == "pending_added")
        .map(|e| (e.ids, e.line))
        .collect()
}

fn claim(r: &mut Rdy, view: &str, spec: &Spec, body: &str) {
    put(
        r,
        view,
        json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(), "body": body}]),
    );
}

#[test]
fn di11_a_new_proposal_version_adds_pending_added() {
    let (_dir, mut r) = project();
    let spec = spec_doc(&mut r);
    let sub = watcher_at_head(&r);
    claim(&mut r, "v1", &spec, "Guests read.");
    assert!(
        pending_added(&r, &sub).is_empty(),
        "a write is not a proposal"
    );
    let p1 = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    claim(&mut r, "v1", &spec, "Guests may not write.");
    let p2 = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    assert_eq!(p2.version, 2, "{p2:?}");
    // every session matches it, the proposer's own too
    let own = InboxSub {
        actor: "agent:v1".into(),
        ..sub.clone()
    };
    for s in [&sub, &own] {
        assert_eq!(
            pending_added(&r, s),
            vec![
                (
                    vec![p1.id.clone()],
                    format!("{} waits for an answer; read rdy_pending", p1.r#ref)
                ),
                (
                    vec![p2.id.clone()],
                    format!("{} waits for an answer; read rdy_pending", p2.r#ref)
                ),
            ],
            "{}",
            s.actor
        );
    }
    assert_ne!(p1.r#ref, p2.r#ref);
    assert!(
        INBOX_DOC.contains("pending_added"),
        "rdy_schema names the entry"
    );
}

#[test]
fn di11_an_advised_verdict_adds_pending_added() {
    let (_dir, mut r) = project();
    let spec = spec_doc(&mut r);
    let q = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$q", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec."}]),
        ),
        "$q",
    );
    let p = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    let sub = watcher_at_head(&r);
    // a verdict with no advice puts nothing in the queue
    let plain = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$p", "parent": spec.section.to_string(),
                    "body": "Guests read the rules."}]),
        ),
        "$p",
    );
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &plain,
        &spec.c,
        "agent:r",
        false,
    );
    assert!(
        pending_added(&r, &sub).is_empty(),
        "no advice: nothing waits"
    );
    // a verdict that advises blocking waits for the person, on the version that holds it
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &q,
        &spec.c,
        "agent:r",
        true,
    );
    let entries = pending_added(&r, &sub);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let (ids, line) = &entries[0];
    assert_eq!(ids.len(), 1, "{ids:?}");
    let review: rdy_model::Hash = ids[0].parse().expect("the id is the review");
    assert!(r.read().reader().review(&review).is_ok(), "{ids:?}");
    assert_eq!(
        line,
        &format!("{} waits for an answer; read rdy_pending", p.r#ref)
    );
}

#[test]
fn di11_pending_added_is_stable_across_reindex() {
    let (dir, mut r) = project();
    let spec = spec_doc(&mut r);
    let q = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$q", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec."}]),
        ),
        "$q",
    );
    let sub = watcher_at_head(&r);
    r.put_subscription(&sub).expect("subscribe");
    let p = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &q,
        &spec.c,
        "agent:r",
        true,
    );
    let before = derived(&r).expect("derived");
    let entries = &before.inboxes[&format!("{WATCHER}/")];
    let added: Vec<_> = entries
        .iter()
        .filter(|e| e["event"] == "pending_added")
        .collect();
    assert_eq!(
        added.len(),
        2,
        "the version and the advised verdict: {entries:?}"
    );
    assert!(
        added
            .iter()
            .all(|e| e["line"]
                == format!("{} waits for an answer; read rdy_pending", p.r#ref).as_str()),
        "{added:?}"
    );
    r.close().expect("close");
    reindex_reproduces(dir.path()).expect("reindex reproduces the inbox");
}
