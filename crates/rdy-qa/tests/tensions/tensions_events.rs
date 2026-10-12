//! tevt1: each write's batch logs its tension moves (plan M2e2, founder decision C, D-Z0EMP);
//! the live events are the logged ones, with re-keys paired.
use rdy_engine::engine::write_typed;
use rdy_engine::tensions::*;
use rdy_engine::{Engine, EventKind, Local, Scope, Session};
use rdy_model::Role;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use rdy_store::transition::{Cause, TensionMove};
use serde_json::json;

/// The tension moves among `kinds`: (resolved, opened).
fn tension_moves(kinds: &[EventKind]) -> (Vec<&TensionMove>, Vec<&TensionMove>) {
    let resolved = kinds
        .iter()
        .filter_map(|k| match k {
            EventKind::TensionResolved(m) => Some(m),
            _ => None,
        })
        .collect();
    let opened = kinds
        .iter()
        .filter_map(|k| match k {
            EventKind::TensionOpened(m) => Some(m),
            _ => None,
        })
        .collect();
    (resolved, opened)
}

#[test]
fn tevt1_a_collision_opens_rekeys_on_landing_and_resolves() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let rx = r.subscribe();
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    let (_, opened) = tension_moves(&kinds);
    assert_eq!(opened.len(), 1, "{kinds:?}");
    assert_eq!(opened[0].kind, "edit_collision");
    assert_eq!(opened[0].rekey, None);
    let t1 = opened[0].tension;

    land(&mut r, "a").unwrap();
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    let (resolved, opened) = tension_moves(&kinds);
    let gone = resolved
        .iter()
        .find(|m| m.tension == t1 && m.rekey.is_some())
        .expect("the re-key resolves the old tension");
    let t2 = gone.rekey.unwrap();
    assert_eq!(gone.why.cause, Cause::Landed);
    assert!(
        opened
            .iter()
            .any(|m| m.tension == t2 && m.rekey == Some(t1)),
        "{kinds:?}"
    );

    // the rebase recorded b's conflict on A: the resolution names the version b edited
    let base = r
        .read()
        .conflict_on(&Scope::View("b".into()), &s.a)
        .unwrap()
        .expect("b's conflict on A")
        .base
        .expect("a conflict on an update has a base");
    let merged = change_of(&put(
        &mut r,
        "b",
        json!([{"op": "update", "id": s.a.to_string(), "base": base.to_string(), "body": "A merged."}]),
    ));
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    let (resolved, _) = tension_moves(&kinds);
    assert!(
        resolved.iter().any(|m| m.tension == t2
            && m.rekey.is_none()
            && m.why.cause == Cause::Committed
            && m.why.id == Some(merged)),
        "{kinds:?}"
    );
}

#[test]
fn tevt1_each_write_logs_its_moves_once_and_reads_log_nothing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    let rx = r.subscribe();
    put(&mut r, "b", upd(&s.a, "A by b."));
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    assert_eq!(tension_moves(&kinds).1.len(), 1, "{kinds:?}");
    let end = r.store().last_batch_end();
    r.read().tensions().unwrap();
    r.invalidate();
    assert_eq!(r.store().last_batch_end(), end, "nothing moved: no batch");
    assert_eq!(rx.try_iter().count(), 0);
}

#[test]
fn tevt1_the_local_engine_logs_the_moves_in_each_write() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    let e = Local::new(r);
    let b = s.a.clone();
    write_typed(&e, move |r| {
        try_put(r, "b", upd(&b, "A by b.")).map(|w| w.change)
    })
    .unwrap();
    assert!(
        e.read().tensions_built(),
        "the write built the new state's set to log its moves"
    );
}

#[test]
fn tevt1_when_the_last_in_flight_side_lands_the_tension_is_rekeyed_to_canon_not_resolved() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", term(&s.section, "Fee"));
    put(&mut r, "b", term(&s.section, "fee"));
    land(&mut r, "a").unwrap();
    let t = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(change_sides(&t).len(), 1);
    let rx = r.subscribe();
    land(&mut r, "b").unwrap();
    let t2 = one(&r, TensionKind::DuplicateDefinition);
    assert!(change_sides(&t2).is_empty(), "every side is canon: {t2:#?}");
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    let (resolved, opened) = tension_moves(&kinds);
    assert!(
        resolved
            .iter()
            .any(|m| m.tension == t.id && m.rekey == Some(t2.id)),
        "{kinds:?}"
    );
    assert!(
        opened
            .iter()
            .any(|m| m.tension == t2.id && m.rekey == Some(t.id)),
        "{kinds:?}"
    );
    assert!(
        !resolved
            .iter()
            .any(|m| m.tension == t.id && m.rekey.is_none()),
        "not resolved"
    );
}

#[test]
fn tevt1_when_the_last_side_of_a_premise_shift_lands_it_is_resolved_not_rekeyed() {
    // FW-CH6a: no all-canon premise shift; the stale pin is a suspect
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let y = created(&put(&mut r, "b", cite(&s.section, &s.a)), "$y");
    put(&mut r, "a", upd(&s.a, "A changed."));
    land(&mut r, "a").unwrap();
    let t = one_involving(&r, TensionKind::PremiseShift, &y);
    assert_eq!(
        change_sides(&t).len(),
        1,
        "one side still in flight: re-keyed, open"
    );
    // the gate refuses an undismissed blocking shift whose last side lands
    let rev = Session::agent("agent:r".into(), Some("rr".into()), Role::Reviewer).unwrap();
    r.dismiss_tension(&rev, &tension_label(&t.id), "B still holds")
        .unwrap();
    let rx = r.subscribe();
    land(&mut r, "b").unwrap();
    assert!(
        of_kind(&r, TensionKind::PremiseShift).is_empty(),
        "no all-canon premise shift"
    );
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    let (resolved, opened) = tension_moves(&kinds);
    // the dismissal already took it out of the open set: the landing moves nothing of it
    assert!(!resolved.iter().any(|m| m.tension == t.id), "{kinds:?}");
    assert!(
        !opened.iter().any(|m| m.kind == "premise_shift"),
        "{kinds:?}"
    );
}

#[test]
fn tevt1_a_dismissal_keeps_the_tension_and_logs_its_resolution() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let t = one(&r, TensionKind::EditCollision);
    let rx = r.subscribe();
    let rev = Session::agent("agent:r".into(), Some("rr".into()), Role::Reviewer).unwrap();
    r.dismiss_tension(&rev, &tension_label(&t.id), "fine")
        .unwrap();
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    let (resolved, opened) = tension_moves(&kinds);
    assert_eq!(resolved.len(), 1, "{kinds:?}");
    assert_eq!(resolved[0].tension, t.id);
    assert_eq!(resolved[0].why.cause, Cause::Dismissed);
    assert_eq!(resolved[0].why.actor.as_deref(), Some("agent:r"));
    assert!(opened.is_empty(), "{kinds:?}");
    assert!(r.read().tensions().unwrap().get(&t.id).is_some());
}

/// Final review M8: a pack reload changes the tension set without a write; its moves are
/// logged in a batch of its own, of cause `pack_reload`.
#[test]
fn tevt1_a_pack_reload_logs_its_moves_in_its_own_batch() {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let before = one(&r, TensionKind::EditCollision);
    assert_eq!(before.severity, TensionSeverity::Blocking);
    let rx = r.subscribe();
    std::fs::write(
        d.path().join(".rdy/packs/soft.yaml"),
        "name: soft\nversion: \"1\"\nextends: rdy-standard\ntensions:\n  edit_collision: advisory\n",
    )
    .unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: soft\n").unwrap();
    assert!(matches!(
        r.reload_pack().unwrap(),
        rdy_engine::ReloadOutcome::Reloaded
    ));
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    let (resolved, opened) = tension_moves(&kinds);
    assert!(
        resolved
            .iter()
            .any(|m| m.tension == before.id && m.why.cause == Cause::PackReload),
        "{kinds:?}"
    );
    assert!(opened.iter().any(|m| m.severity == "advisory"), "{kinds:?}");
    // the next write's moves carry its own cause and nothing of the reload's
    let rx = r.subscribe();
    put(&mut r, "c", upd(&s.b, "B by c."));
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    let (resolved, _) = tension_moves(&kinds);
    assert!(
        !resolved.iter().any(|m| m.tension == before.id),
        "{kinds:?}"
    );
}

/// A detector that panics.
struct Panics;
impl Detector for Panics {
    fn kind(&self) -> TensionKind {
        TensionKind::EditCollision
    }
    fn detect(
        &self,
        _cx: &DetectCx<'_>,
        _seeds: Option<&std::collections::BTreeSet<rdy_model::BlockId>>,
        _out: &mut Vec<Found>,
    ) -> Result<(), rdy_engine::RdyError> {
        panic!("a detector bug")
    }
}
static PANICS: Panics = Panics;
static PANICS_DETS: [&dyn Detector; 1] = [&PANICS];

/// Final review M7: in the in-process engine (direct-mode CLI, the stdio MCP server) a move
/// computation that panics never reaches the caller of a committed write, and the engine stays
/// usable.
#[test]
fn tevt1_a_settle_panic_never_fails_a_local_write() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.set_detectors_for_tests(&PANICS_DETS);
    let e = Local::new(r);
    let a = s.a.clone();
    let v = write_typed(&e, move |r| try_put(r, "a", upd(&a, "A by a.")));
    assert!(v.is_ok(), "{v:?}");
    let b = s.b.clone();
    let v = write_typed(&e, move |r| try_put(r, "a", upd(&b, "B by a.")));
    assert!(v.is_ok(), "the engine is usable after the panic: {v:?}");
    assert_eq!(e.read().included("a").len(), 2);
}
