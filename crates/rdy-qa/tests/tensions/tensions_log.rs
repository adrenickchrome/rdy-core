//! ib15: each batch logs its tension and suspect moves; the bus sends what a replay reads.
use rdy_engine::tensions::*;
use rdy_engine::{EventKind, Rdy};
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use rdy_store::inbox::{InboxCursor, InboxSub};
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
    // task-15 ruling: the suspects are canon's, so the citer lands first and agent a's
    // in-flight changes write it again
    let a = put(&mut r, "a", cite(&s.section, &s.c));
    land(&mut r, "a").unwrap();
    let y = created(&a, "$y");
    put(&mut r, "a", upd_citing(&y, &s.c));
    write(&mut r, "work", upd(&s.c, "C moved.")).unwrap();
    land(&mut r, "work").unwrap();
    let rec = last_record(&r).expect("the landing's moves");
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
    assert_eq!(m.why.cause, Cause::Landed);
}

/// One update op: `id`'s body, still citing `target`.
fn upd_citing(id: &rdy_model::BlockId, target: &rdy_model::BlockId) -> serde_json::Value {
    serde_json::json!([{"op": "update", "id": id.to_string(),
        "body": format!("Still builds on {{{{{target}}}}}.")}])
}

#[test]
fn ib15_an_inflight_citer_of_a_landed_change_meets_a_premise_shift_not_a_suspect() {
    // task-15 ruling: in flight, a citer of a changed premise meets a premise shift (logged at
    // the commit); a suspect is logged only when a stale citation is in canon
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let a = put(&mut r, "a", cite(&s.section, &s.c));
    let y = created(&a, "$y");
    let w = write(&mut r, "work", upd(&s.c, "C moved.")).unwrap();
    let rec = last_record(&r).expect("the commit's moves");
    assert!(
        rec.new.is_empty() && rec.gone.is_empty(),
        "no canon change: {rec:?}"
    );
    let shift = |rec: &Transitions| {
        rec.opened
            .iter()
            .find(|m| m.kind == "premise_shift")
            .map(|m| m.why.clone())
    };
    let why = shift(&rec).expect("a premise shift");
    assert_eq!((why.cause, why.id), (Cause::Committed, Some(change_of(&w))));
    land(&mut r, "work").unwrap();
    let rec = last_record(&r).expect("the landing's moves");
    assert!(
        rec.new.iter().chain(&rec.gone).all(|m| m.block != y),
        "{rec:?}"
    );
    assert!(
        r.read()
            .suspects(&rdy_engine::Scope::Canon)
            .unwrap()
            .is_empty()
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
    let at = r.read().seq();
    assert!(r.reconcile_transitions().unwrap() > 0);
    let rec = last_record(&r).unwrap();
    assert!(
        rec.opened.iter().all(|m| m.why.cause == Cause::Reopened),
        "{rec:?}"
    );
    // final review fix round 1 (Important 2): the severity change re-keys each tension, as the
    // live reload does; the reconcile pairs them by the second re-key rule
    assert!(!rec.opened.is_empty(), "{rec:?}");
    assert_eq!(rec.opened.len(), rec.resolved.len(), "{rec:?}");
    for m in &rec.opened {
        let back = m.rekey.expect("every opening re-keys a resolved tension");
        let o = rec.resolved.iter().find(|o| o.tension == back);
        assert_eq!(o.and_then(|o| o.rekey), Some(m.tension), "{rec:?}");
    }
    // the inbox says `replaces`, never that the tension left a's in-flight changes
    let sub = InboxSub {
        actor: "agent:a".into(),
        run: Some("a".into()),
        cursor: InboxCursor { at, skip: 0 },
        areas: vec![],
        role: "writer".into(),
    };
    let page = r.read().inbox_page(&sub, None, usize::MAX).unwrap();
    let kinds: Vec<&str> = page.entries.iter().map(|e| e.event).collect();
    assert_eq!(kinds, ["tension_opened"], "{page:?}");
    assert!(
        page.entries[0].line.contains(" replaces ")
            && page.entries[0].line.ends_with(
                "(edit_collision, advisory): the pack or rdy changed while the project was closed"
            ),
        "{}",
        page.entries[0].line
    );
    assert_eq!(r.reconcile_transitions().unwrap(), 0);
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
}

/// Final review fix round 1 (Minor 8): a pack reload before anything is logged since open logs
/// the moves made while the project was closed as `reopened`, not as the reload's.
#[test]
fn ib15_a_reload_before_the_first_logged_batch_keeps_the_reopened_cause() {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    r.close().unwrap();
    let soft = |v: &str| {
        format!(
            "name: soft\nversion: \"{v}\"\nextends: rdy-standard\ntensions:\n  edit_collision: advisory\n"
        )
    };
    std::fs::write(d.path().join(".rdy/packs/soft.yaml"), soft("1")).unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: soft\n").unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    // the pack changes again before the reconcile, in a way that moves no tension
    std::fs::write(d.path().join(".rdy/packs/soft.yaml"), soft("2")).unwrap();
    assert!(matches!(
        r.reload_pack().unwrap(),
        rdy_engine::ReloadOutcome::Reloaded
    ));
    let rec = last_record(&r).expect("the reload's batch logs the moves since open");
    assert!(!rec.opened.is_empty(), "{rec:?}");
    assert!(
        rec.resolved
            .iter()
            .chain(&rec.opened)
            .all(|m| m.why.cause == Cause::Reopened),
        "{rec:?}"
    );
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
    // task-15 ruling: the in-flight suspects of the spec's writes are no longer logged; the
    // collision's opening and the landing's moves are
    assert!(f.transitions >= 2, "{f:?}");
    assert_eq!(f.unlogged_moves, 0);
}

/// The presentational pack of the render tests, with `aside` given `traits`.
fn aside_pack(traits: &str) -> String {
    rdy_qa::golden::PRESENTATIONAL_PACK.replace("traits: [presentational]", traits)
}

#[test]
fn ib15_a_reload_that_makes_a_cited_type_informative_logs_its_suspect() {
    // review fix round 1 (finding 1): the suspects of canon read the pack's presentational trait
    let (d, r) = project();
    drop(r);
    std::fs::write(
        d.path().join(".rdy/packs/p.yaml"),
        aside_pack("traits: [presentational]"),
    )
    .unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: p\n").unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    let res = write(&mut r, "work", serde_json::json!([
        {"op":"create","type":"doc","id":"$d","fields":{"name":"t","title":"T","doc_type":"page"}},
        {"op":"create","type":"section","id":"$s","parent":"$d","fields":{"title":"Terms"}},
        {"op":"create","type":"aside","id":"$a","parent":"$s","body":"Decorative."},
        {"op":"create","type":"claim","id":"$c","parent":"$s","body":"Depends on {{$a}}.",
         "links":{"depends_on":[{"target":"$a"}]}}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let (aside, claim) = (created(&res, "$a"), created(&res, "$c"));
    write(&mut r, "work", upd(&aside, "Still decorative.")).unwrap();
    land(&mut r, "work").unwrap();
    assert!(
        r.read()
            .suspects(&rdy_engine::Scope::Canon)
            .unwrap()
            .is_empty()
    );
    std::fs::write(
        d.path().join(".rdy/packs/p.yaml"),
        aside_pack("traits: [informative]"),
    )
    .unwrap();
    assert!(matches!(
        r.reload_pack().unwrap(),
        rdy_engine::ReloadOutcome::Reloaded
    ));
    assert_eq!(r.store().last_batch().len(), 1, "only the record");
    let rec = last_record(&r).unwrap();
    let m = rec
        .new
        .iter()
        .find(|m| m.block == claim)
        .expect("the claim is suspect now");
    assert_eq!(
        (m.target.clone(), m.why.cause),
        (aside.clone(), Cause::PackReload)
    );
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
    // a later move of that suspect folds
    write(
        &mut r,
        "work",
        serde_json::json!([{"op": "delete", "id": claim.to_string()}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let rec = last_record(&r).unwrap();
    assert!(
        rec.gone
            .iter()
            .any(|m| m.block == claim && m.why.cause == Cause::Landed),
        "{rec:?}"
    );
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
}

/// A pack whose claims carry a field `note`, tracked or not.
fn note_pack(tracked: bool) -> String {
    format!(
        "name: trk\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  claim:\n    prefix: c\n    traits: [normative, describes]\n    body: required\n    fields:\n      note: {{ kind: str }}\n{}",
        if tracked {
            "    tracking: [note]\n"
        } else {
            ""
        }
    )
}

#[test]
fn ib15_a_reload_that_tracks_a_field_reopens_a_dismissed_tension_and_logs_it() {
    // review fix round 1 (finding 2): the tension keeps its id, its vkey moves, its dismissal lapses
    let (d, r) = project();
    drop(r);
    std::fs::write(d.path().join(".rdy/packs/trk.yaml"), note_pack(false)).unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    let s = spec_doc(&mut r);
    let note = |body: &str, note: &str| {
        serde_json::json!([{"op": "update", "id": s.a.to_string(), "body": body,
            "fields": {"note": note}}])
    };
    put(&mut r, "a", note("A by a.", "na"));
    put(&mut r, "b", note("A by b.", "nb"));
    let t = one(&r, TensionKind::EditCollision);
    r.dismiss_tension(&writer(), &tension_label(&t.id), "fine")
        .unwrap();
    std::fs::write(d.path().join(".rdy/packs/trk.yaml"), note_pack(true)).unwrap();
    assert!(matches!(
        r.reload_pack().unwrap(),
        rdy_engine::ReloadOutcome::Reloaded
    ));
    let t2 = one(&r, TensionKind::EditCollision);
    assert_eq!(t2.id, t.id);
    assert_ne!(t2.vkey, t.vkey);
    assert_eq!(r.read().tension_status(&t2), TensionStatus::Open);
    let rec = last_record(&r).expect("the reload's moves");
    assert!(
        rec.opened
            .iter()
            .any(|m| m.tension == t.id && m.why.cause == Cause::PackReload),
        "{rec:?}"
    );
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
    // a later move of that tension folds
    put(
        &mut r,
        "b",
        serde_json::json!([{"op": "delete", "id": s.a.to_string()}]),
    );
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
}
