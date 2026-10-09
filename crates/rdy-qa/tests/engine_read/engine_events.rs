//! evt1 (spec §10.1): every committed batch emits events after the state swap; a replay from the
//! log reproduces the live stream exactly; nothing is emitted twice.
use rdy_engine::events::{Event, EventKind};
use rdy_qa::fixtures::{approver, land, project, write};
use serde_json::json;
use std::sync::Arc;

fn drain(rx: &std::sync::mpsc::Receiver<Arc<Event>>) -> Vec<Event> {
    let mut out = vec![];
    while let Ok(e) = rx.try_recv() {
        out.push((*e).clone());
    }
    out
}

#[test]
fn evt1_live_events_equal_the_replay_and_each_write_emits_after_its_state() {
    let (_d, mut r) = project();
    let from = r.read().seq();
    let rx = r.subscribe();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}])).unwrap();
    let first = drain(&rx);
    assert_eq!(first.len(), 2, "{first:?}");
    assert_eq!(
        first[0].seq,
        r.read().seq(),
        "the event carries the state's seq"
    );
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "b", "title": "B", "doc_type": "glossary"}}])).unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(&mut r, "other", json!([{"op": "create", "type": "doc", "fields": {"name": "c", "title": "C", "doc_type": "glossary"}}])).unwrap();
    let mut live = first;
    live.extend(drain(&rx));
    let replay = r.replay_events(from).unwrap();
    assert_eq!(live, replay);
    let names: Vec<&str> = live
        .iter()
        .map(|e| match &e.kind {
            EventKind::ViewOp { kind, .. } => kind.as_str(),
            EventKind::ChangeCommitted { .. } => "change_committed",
            EventKind::Proposed { .. } => "proposed",
            EventKind::Landed { .. } => "landed",
            EventKind::ViewRebased { .. } => "view_rebased",
            EventKind::ViewClosed { .. } => "view_closed",
            EventKind::IntentPosted { .. } => "intent_posted",
            EventKind::AltSetChanged { .. } => "alt_set_changed",
            EventKind::TensionDismissed { .. } => "tension_dismissed",
            EventKind::TensionOpened { .. } => "tension_opened",
            EventKind::TensionResolved { .. } => "tension_resolved",
        })
        .collect();
    assert_eq!(
        names,
        [
            "create",
            "change_committed",
            "change_committed",
            "proposed",
            "landed",
            "view_rebased",
            "create",
            "change_committed"
        ],
        "{names:?}"
    );
    assert!(live.windows(2).all(|w| w[0].seq <= w[1].seq));
}

#[test]
fn evt1_invalidate_never_re_emits_the_last_batch() {
    let (_d, mut r) = project();
    let rx = r.subscribe();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}])).unwrap();
    assert_eq!(drain(&rx).len(), 2);
    r.invalidate();
    r.invalidate();
    assert!(drain(&rx).is_empty());
    let stale = json!([{"op": "update", "id": r.read().materialized(&rdy_engine::Scope::View("work".into())).unwrap().ids().next().unwrap().to_string(), "base": rdy_model::Hash([3; 32]).to_string(), "fields": {"title": "X"}}]);
    assert!(write(&mut r, "work", stale).is_err());
    assert!(drain(&rx).is_empty(), "a refused write emits nothing");
}

#[test]
fn evt1_a_reopened_project_replays_what_it_missed_and_emits_only_new_batches() {
    let dir = tempfile::tempdir().unwrap();
    let from;
    {
        let mut r = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
        from = r.read().seq();
        write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}])).unwrap();
    }
    let mut r = rdy_engine::Rdy::open(dir.path()).unwrap();
    let rx = r.subscribe();
    assert_eq!(r.replay_events(from).unwrap().len(), 2);
    assert!(drain(&rx).is_empty(), "nothing before open is emitted live");
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "b", "title": "B", "doc_type": "glossary"}}])).unwrap();
    assert_eq!(
        drain(&rx).len(),
        1,
        "a commit into an existing view: one event"
    );
}
