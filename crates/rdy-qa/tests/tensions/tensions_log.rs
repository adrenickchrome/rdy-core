//! ib15: each batch logs its tension and suspect moves; the bus sends what a replay reads.
use rdy_engine::tensions::*;
use rdy_engine::{EventKind, Rdy};
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::transition::{Cause, Transitions};
use std::collections::BTreeSet;

/// The record that ends the store's last batch, if it has one.
fn last_record(r: &Rdy) -> Option<Transitions> {
    let (k, h) = *r.store().last_batch().last()?;
    (k == kinds::TRANSITION).then(|| r.read().reader().transitions(&h).unwrap())
}
fn markers(r: &Rdy, from: Location) -> usize {
    let mut n = 0;
    r.read()
        .reader()
        .for_each_frame_from(from, r.read().seq(), |_, k, _, _, _| {
            if k == kinds::BATCH_COMMIT {
                n += 1;
            }
            Ok(())
        })
        .unwrap();
    n
}

#[test]
fn ib15_a_collision_is_logged_in_the_batch_of_the_write_that_opened_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    let start = r.read().seq();
    let b = put(&mut r, "b", upd(&s.a, "A by b."));
    let rec = last_record(&r).expect("the write's batch ends with its moves");
    assert_eq!(rec.opened.len(), 1, "{rec:?}");
    let m = &rec.opened[0];
    assert_eq!(m.kind, "edit_collision");
    assert_eq!(
        (m.why.cause, m.why.id),
        (Cause::Committed, Some(change_of(&b)))
    );
    let authors: BTreeSet<&str> = m.sides.iter().filter_map(|s| s.actor.as_deref()).collect();
    assert_eq!(authors, BTreeSet::from(["agent:a", "agent:b"]));
    assert_eq!(markers(&r, start), 1, "one batch: the write and its moves");
}

#[test]
fn ib15_a_landing_rekeys_under_its_landing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let t1 = one(&r, TensionKind::EditCollision).id;
    land(&mut r, "a").unwrap();
    let rec = last_record(&r).expect("the landing's moves");
    let gone = rec
        .resolved
        .iter()
        .find(|m| m.tension == t1)
        .expect("t1 resolved");
    assert_eq!(gone.why.cause, Cause::Landed);
    let t2 = gone.rekey.expect("a re-key");
    assert!(
        rec.opened
            .iter()
            .any(|m| m.tension == t2 && m.rekey == Some(t1)),
        "{rec:?}"
    );
}

#[test]
fn ib15_a_dismissal_resolves_with_its_reviewer() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let t = one(&r, TensionKind::EditCollision);
    r.dismiss_tension(&writer(), &tension_label(&t.id), "fine")
        .unwrap();
    let rec = last_record(&r).expect("the dismissal's move");
    assert_eq!(rec.resolved.len(), 1, "{rec:?}");
    let w = &rec.resolved[0].why;
    assert_eq!(
        (w.cause, w.actor.as_deref()),
        (Cause::Dismissed, Some(writer().actor.as_str()))
    );
}

#[test]
fn ib15_a_cited_block_that_changes_logs_a_suspect_with_its_writers() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let a = put(&mut r, "a", cite(&s.section, &s.c));
    // the suspects of a state are its in-flight scope's: C's change makes the citer suspect when
    // it is committed, not when it lands
    let w = write(&mut r, "work", upd(&s.c, "C moved.")).unwrap();
    let rec = last_record(&r).expect("the write's moves");
    let m = rec
        .new
        .iter()
        .find(|m| m.target == s.c)
        .expect("a suspect on C's citer");
    assert_eq!(m.block, created(&a, "$y"));
    assert_eq!(m.reason, "changed");
    assert_eq!(
        m.writers
            .iter()
            .map(|w| w.actor.as_str())
            .collect::<Vec<_>>(),
        ["agent:a"]
    );
    assert_eq!(
        (m.why.cause, m.why.id),
        (Cause::Committed, Some(change_of(&w)))
    );
    land(&mut r, "work").unwrap();
    let rec = last_record(&r).expect("the landing's moves");
    assert!(
        rec.new.iter().chain(&rec.gone).all(|m| m.target != s.c),
        "logged once: {rec:?}"
    );
}

#[test]
fn ib15_a_write_that_moves_nothing_logs_nothing() {
    let (_d, mut r) = project();
    spec_doc(&mut r);
    put(
        &mut r,
        "a",
        serde_json::json!([{"op": "create", "type": "doc", "id": "$d",
        "fields": {"name": "x", "title": "X", "doc_type": "overview"}}]),
    );
    assert!(last_record(&r).is_none());
}

#[test]
fn ib15_the_bus_sends_what_a_replay_reads() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let from = r.read().seq();
    let rx = r.subscribe();
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    land(&mut r, "a").unwrap();
    let live: Vec<_> = rx.try_iter().map(|e| (*e).clone()).collect();
    assert!(
        live.iter()
            .any(|e| matches!(e.kind, EventKind::TensionOpened(_)))
    );
    assert_eq!(live, r.replay_events(from).unwrap());
}

#[test]
fn ib15_a_pack_reload_writes_a_batch_of_its_own() {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let before = one(&r, TensionKind::EditCollision).id;
    std::fs::write(d.path().join(".rdy/packs/soft.yaml"),
        "name: soft\nversion: \"1\"\nextends: rdy-standard\ntensions:\n  edit_collision: advisory\n").unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: soft\n").unwrap();
    assert!(matches!(
        r.reload_pack().unwrap(),
        rdy_engine::ReloadOutcome::Reloaded
    ));
    assert_eq!(r.store().last_batch().len(), 1, "only the record");
    let rec = last_record(&r).unwrap();
    assert!(
        rec.resolved
            .iter()
            .any(|m| m.tension == before && m.why.cause == Cause::PackReload),
        "{rec:?}"
    );
    assert!(
        rec.opened
            .iter()
            .all(|m| m.why.cause == Cause::PackReload && m.severity == "advisory")
    );
}

#[test]
fn ib15_a_project_reopened_under_another_pack_logs_reopened_moves() {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    r.close().unwrap();
    std::fs::write(d.path().join(".rdy/packs/soft.yaml"),
        "name: soft\nversion: \"1\"\nextends: rdy-standard\ntensions:\n  edit_collision: advisory\n").unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: soft\n").unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    assert!(r.fsck().unwrap().unlogged_moves > 0);
    assert!(r.reconcile_transitions().unwrap() > 0);
    assert!(
        last_record(&r)
            .unwrap()
            .opened
            .iter()
            .all(|m| m.why.cause == Cause::Reopened)
    );
    assert_eq!(r.reconcile_transitions().unwrap(), 0);
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
}

#[test]
fn ib15_reindex_rebuilds_the_tables_and_the_replay() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let from = r.read().seq();
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    land(&mut r, "a").unwrap();
    let events = r.replay_events(from).unwrap();
    let open = r.read().reader().open_tensions().unwrap();
    r.reindex().unwrap();
    assert_eq!(r.replay_events(from).unwrap(), events);
    assert_eq!(r.read().reader().open_tensions().unwrap(), open);
    let f = r.fsck().unwrap();
    assert!(f.transitions >= 3, "{f:?}");
    assert_eq!(f.unlogged_moves, 0);
}
