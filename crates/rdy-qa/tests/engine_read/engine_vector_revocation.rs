//! vxs12: vector similarity is advice only. Lexically similar content adds no review request, no
//! inbox entry and nothing a reviewer can claim or submit without a lease: the queue holds only
//! the requests of the five fixed local rules.
use rdy_engine::brief::Coverage;
use rdy_engine::inbox::Entry;
use rdy_engine::inbox::matcher::match_event;
use rdy_engine::semantic::api::{ReviewNextRequest, run_review_next};
use rdy_engine::semantic::candidates::{Rule, request_label};
use rdy_engine::{Local, Rdy, Scope, Session, pin_clock};
use rdy_model::{BlockId, Role};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use rdy_store::inbox::{InboxCursor, InboxSub};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The words every similar claim holds: rare in the project, so they score high.
const SIMILAR: &str = "Quokka lemur gateway ledger.";

fn reviewer(actor: &str) -> Session {
    Session::agent(actor.into(), Some("r".into()), Role::Reviewer).unwrap()
}

fn next(e: &Local, s: &Session, owner: u64, kind: &str) -> Value {
    run_review_next(e, s, owner, ReviewNextRequest { kind: kind.into() }).unwrap()
}

/// A reviewer's inbox subscription at the head of the log.
fn sub_at_head(r: &Rdy, s: &Session) -> InboxSub {
    InboxSub {
        actor: s.actor.clone(),
        run: s.run.clone(),
        cursor: InboxCursor {
            at: r.read().seq(),
            skip: 0,
        },
        areas: vec![],
        role: "reviewer".into(),
    }
}

fn entries(r: &Rdy, sub: &InboxSub) -> Vec<Entry> {
    let st = r.read();
    let infl = st.run_in_flight(&sub.actor, sub.run.as_deref()).unwrap();
    st.replay(sub.cursor.at)
        .unwrap()
        .iter()
        .filter_map(|e| match_event(&st, sub, &infl, e).unwrap())
        .collect()
}

/// Canon: doc `north` with claim `a` and its section sibling `s`; doc `south` with claim `b`,
/// whose text is `a`'s. No link, entity, shared section or area joins `a` and `b`.
fn similar_docs(r: &mut Rdy) -> (BlockId, BlockId, BlockId) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "north", "title": "North", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$ns", "parent": "$n", "fields": {"title": "Rules"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$ns", "body": SIMILAR},
        {"op": "create", "type": "claim", "id": "$s", "parent": "$ns", "body": "Sibling stands alone."},
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "south", "title": "South", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$os", "parent": "$o", "fields": {"title": "Rules"}},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$os", "body": SIMILAR}
    ]))
    .expect("the similar docs");
    let ids = (created(&w, "$a"), created(&w, "$s"), created(&w, "$b"));
    land(r, "work").expect("land the docs");
    ids
}

#[test]
fn vxs12_similarity_adds_no_review_request() {
    let (_d, mut r) = project();
    let (a, sibling, b) = similar_docs(&mut r);
    let rv = reviewer("agent:rv");
    let inbox = sub_at_head(&r, &rv);
    let _clock = pin_clock(1_000);
    let w = put(&mut r, "x", upd(&a, "Quokka lemur gateway ledger, kept."));
    let change = change_of(&w);

    // the advice sees b: the similarity is there to be reported
    let st = r.read();
    let cov = Coverage {
        ids: vec![a.to_string()],
        path: None,
    };
    let brief = st
        .brief(&Scope::View("x".into()), &cov, Some(1_000_000))
        .unwrap();
    assert!(
        brief
            .vector_neighbors
            .iter()
            .any(|v| v.r#for == a.to_string() && v.block == b.to_string()),
        "{:#?}",
        brief.vector_neighbors
    );

    // the queue holds the one section request, and nothing names b
    let q = st.review_requests().unwrap();
    assert_eq!(q.len(), 1, "{q:#?}");
    assert_eq!(q[0].rule, Rule::Section);
    assert_eq!(q[0].change, Some(change));
    let pair: BTreeSet<_> = q[0].items.iter().map(|i| i.block.clone()).collect();
    assert_eq!(pair, BTreeSet::from([a.clone(), sibling.clone()]));
    let from = st.requests_from(&change).unwrap();
    assert_eq!(
        from.iter().map(|x| x.key).collect::<Vec<_>>(),
        vec![q[0].key]
    );
    assert!(
        q.iter()
            .chain(&from)
            .all(|x| x.items.iter().all(|i| i.block != b))
    );
    // a lease-less submit resolves a derived request from this queue: b has none to resolve
    let leased = BTreeSet::from([q[0].key]);
    assert_eq!(st.next_request("agent:rv", &leased).unwrap(), None);
    drop(st);

    // one inbox entry, counting the one request
    let es = entries(&r, &inbox);
    assert_eq!(
        es.iter().map(|e| e.event).collect::<Vec<_>>(),
        ["review_requested"]
    );
    assert_eq!(
        es[0].line,
        format!(
            "1 review requests from change {}: call rdy_review_next",
            change.short()
        )
    );

    // a reviewer claims the one request; the next reviewer finds nothing more, of any kind
    let e = Local::new(r);
    let one = next(&e, &reviewer("agent:r1"), 1, "tension");
    assert_eq!(one["request"], json!(request_label(&q[0].key)));
    let ids: BTreeSet<String> = one["case"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, BTreeSet::from([a.to_string(), sibling.to_string()]));
    for kind in ["tension", "any"] {
        let none = next(&e, &reviewer("agent:r2"), 2, kind);
        assert_eq!(none, json!({"request": null, "leased": 1}), "{kind}");
    }
}
