//! evt1: events derive from a batch's frames, and the bus fans them out.
use rdy_engine::events::{Event, EventKind, Events, events_for_batch};
use rdy_engine::{Rdy, Session};
use rdy_model::Role;
use rdy_store::kinds;
use std::sync::Arc;

fn writer() -> Session {
    Session {
        actor: "a".into(),
        run: Some("r".into()),
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn one_doc(r: &mut Rdy, view: &str, name: &str) {
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}
    ]))
    .unwrap();
    r.write(
        &writer(),
        rdy_engine::WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
}

#[test]
fn evt1_a_first_write_into_a_view_gives_a_create_op_then_a_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    one_doc(&mut r, "v", "d");
    let frames = r.store().last_batch().to_vec();
    let seq = r.store().last_batch_end();
    let evs = events_for_batch(&r.store().reader(), seq, &frames).unwrap();
    let kinds: Vec<&EventKind> = evs.iter().map(|e| &e.kind).collect();
    assert_eq!(evs.len(), 2, "{evs:?}");
    assert!(
        matches!(kinds[0], EventKind::ViewOp { view, kind, .. } if view == "v" && kind == "create")
    );
    assert!(
        matches!(kinds[1], EventKind::ChangeCommitted { view, actor, run, .. } if view == "v" && actor == "a" && run.as_deref() == Some("r"))
    );
    assert!(evs.iter().all(|e| e.seq == seq));
    assert!(evs.iter().all(|e| e.at_ms > 0));
}

#[test]
fn evt1_a_landing_batch_gives_commit_proposed_landed_and_rebased() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "log", "title": "L", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$l", "fields": {"title": "Decisions"}}
    ]))
    .unwrap();
    let res = r
        .write(
            &writer(),
            rdy_engine::WriteRequest {
                view: "v".into(),
                ops,
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    r.decide(
        &writer(),
        "v",
        rdy_engine::DecisionInput {
            parent: res.created["$s"].clone(),
            statement: "S".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let p = r.propose(&writer(), "v", "p").unwrap();
    let pf = r.store().last_batch().to_vec();
    let pe = events_for_batch(&r.store().reader(), r.store().last_batch_end(), &pf).unwrap();
    assert!(
        matches!(&pe[..], [Event { kind: EventKind::Proposed { view, .. }, .. }] if view == "v"),
        "{pe:?}"
    );
    let approver = Session {
        actor: "h".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    };
    r.land(&approver, &p.id).unwrap();
    let lf = r.store().last_batch().to_vec();
    let le = events_for_batch(&r.store().reader(), r.store().last_batch_end(), &lf).unwrap();
    let names: Vec<&str> = le
        .iter()
        .map(|e| match &e.kind {
            EventKind::ChangeCommitted { .. } => "change_committed",
            EventKind::Proposed { .. } => "proposed",
            EventKind::Landed { .. } => "landed",
            EventKind::ViewRebased { .. } => "view_rebased",
            EventKind::ViewClosed { .. } => "view_closed",
            EventKind::ViewOp { .. } => "view_op",
            EventKind::IntentPosted { .. } => "intent_posted",
            EventKind::AltSetChanged { .. } => "alt_set_changed",
        })
        .collect();
    // the acceptance change, the final proposal version, the landing, the proposing view's rebase
    assert_eq!(
        names,
        ["change_committed", "proposed", "landed", "view_rebased"],
        "{le:?}"
    );
}

#[test]
fn evt1_frames_of_other_kinds_give_no_event_and_a_missing_object_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let h = r.store_mut().put_blob(b"x").unwrap();
    let seq = r.store().last_batch_end();
    assert!(
        events_for_batch(&r.store().reader(), seq, &[(kinds::BLOB_MANIFEST, h)])
            .unwrap()
            .is_empty()
    );
    let e = events_for_batch(
        &r.store().reader(),
        seq,
        &[(kinds::VIEW_OP, rdy_model::Hash([1; 32]))],
    )
    .unwrap_err();
    assert_eq!(e.code, rdy_engine::E_NOT_FOUND);
}

#[test]
fn evt1_the_bus_delivers_to_every_live_subscriber_and_forgets_dropped_ones() {
    let bus = Events::new();
    let a = bus.subscribe();
    let b = bus.subscribe();
    let ev = Arc::new(Event {
        seq: rdy_store::segment::Location {
            segment: 0,
            offset: 7,
        },
        at_ms: 1,
        kind: EventKind::ViewOp {
            view: "v".into(),
            op: rdy_model::Hash([2; 32]),
            kind: "close".into(),
        },
    });
    bus.publish(Arc::clone(&ev));
    assert_eq!(*a.recv().unwrap(), *ev);
    assert_eq!(*b.recv().unwrap(), *ev);
    drop(b);
    bus.publish(Arc::clone(&ev));
    assert_eq!(bus.subscriber_count(), 1);
    assert_eq!(*a.recv().unwrap(), *ev);
}

/// A batch of one undecodable proposal: the store indexes it (it decodes no proposal to index),
/// but its events cannot be read.
fn junk_batch(r: &mut Rdy, bytes: &[u8]) -> rdy_model::Hash {
    r.store_mut()
        .write(vec![(kinds::PROPOSAL, bytes.to_vec())])
        .unwrap()[0]
}

#[test]
fn evt1_a_batch_whose_events_cannot_be_read_is_recorded_and_never_fails_the_publish() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let rx = r.subscribe();
    junk_batch(&mut r, b"junk");
    // the reindex succeeded; the events of the batch before it are not its result
    r.reindex().unwrap();
    let e = r
        .last_emit_error()
        .expect("the failure is recorded")
        .message
        .clone();
    assert_eq!(r.last_emitted_seq(), r.store().last_batch_end());
    assert!(rx.try_recv().is_err());
    // a later batch emits its own events and never masks the earlier failure
    one_doc(&mut r, "v", "d");
    let live: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|e| (*e).clone())
        .collect();
    assert_eq!(live.len(), 2, "{live:?}");
    assert_eq!(r.last_emit_error().map(|e| &e.message), Some(&e));
}

/// A batch of one landing whose proposal is missing: the store indexes it, but its `Landed`
/// event cannot be read.
fn orphan_landing(r: &mut Rdy, proposal: rdy_model::Hash) {
    let rec = rdy_model::LandingRecord {
        proposal,
        prev: None,
        state: rdy_model::Hash([0; 32]),
        approved_by: rdy_model::Provenance {
            actor: "a".into(),
            run: None,
            role: Role::Approver,
            at_ms: 1,
        },
        changes: vec![],
        chosen: None,
        checks: vec![],
        assurance: rdy_model::Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        approved_version: None,
        rebased_from: vec![],
        evidence: None,
    };
    r.store_mut().put(kinds::LANDING, &rec).unwrap();
}

#[test]
fn evt1_replay_stops_at_the_first_batch_whose_events_cannot_be_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let from = r.read().seq();
    let first = rdy_model::Hash([1; 32]);
    let second = rdy_model::Hash([2; 32]);
    orphan_landing(&mut r, first);
    orphan_landing(&mut r, second);
    r.invalidate();
    let e = r.replay_events(from).unwrap_err();
    assert!(
        e.message.contains(&first.short()) && !e.message.contains(&second.short()),
        "{e}"
    );
}
