//! tevt1: tension events are the diff of consecutive settled sets, with re-keys paired.
use rdy_engine::engine::write_typed;
use rdy_engine::tensions::*;
use rdy_engine::{Engine, EventKind, Local, Scope, Session};
use rdy_model::Role;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::json;

#[test]
fn tevt1_a_collision_opens_rekeys_on_landing_and_resolves() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.settle_tensions().unwrap();
    let rx = r.subscribe();
    put(&mut r, "a", upd(&s.a, "A by a."));
    r.settle_tensions().unwrap();
    put(&mut r, "b", upd(&s.a, "A by b."));
    r.settle_tensions().unwrap();
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    let opened: Vec<_> = kinds
        .iter()
        .filter_map(|k| match k {
            EventKind::TensionOpened {
                tension,
                kind,
                rekeyed_from,
                ..
            } => Some((*tension, kind.clone(), *rekeyed_from)),
            _ => None,
        })
        .collect();
    assert_eq!(opened.len(), 1, "{kinds:?}");
    assert_eq!(opened[0].1, "edit_collision");
    assert_eq!(opened[0].2, None);
    let t1 = opened[0].0;

    land(&mut r, "a").unwrap();
    r.settle_tensions().unwrap();
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    let (t2, by) = kinds
        .iter()
        .find_map(|k| match k {
            EventKind::TensionResolved {
                tension,
                rekeyed_to: Some(to),
                by,
                ..
            } if *tension == t1 => Some((*to, by.clone())),
            _ => None,
        })
        .expect("the re-key resolves the old tension");
    assert!(by.unwrap().starts_with("landing:"));
    assert!(kinds.iter().any(|k| matches!(k,
        EventKind::TensionOpened { tension, rekeyed_from: Some(f), .. } if *tension == t2 && *f == t1)));

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
    r.settle_tensions().unwrap();
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    assert!(
        kinds.iter().any(|k| matches!(k,
        EventKind::TensionResolved { tension, rekeyed_to: None, by: Some(b), .. }
            if *tension == t2 && *b == format!("change:{merged}"))),
        "{kinds:?}"
    );
}

#[test]
fn tevt1_settling_twice_emits_nothing_and_reads_never_settle() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    assert!(!r.read().tensions_built(), "no read built the set yet");
    assert_eq!(
        r.settle_tensions().unwrap(),
        1,
        "the first write's state is the baseline"
    );
    assert_eq!(r.settle_tensions().unwrap(), 0);
}

#[test]
fn tevt1_the_local_engine_settles_after_each_write() {
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
        "settle built the new state's set on the writer"
    );
}

#[test]
fn tevt1_when_the_last_in_flight_side_lands_the_tension_is_rekeyed_to_canon_not_resolved() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", term(&s.section, "Fee"));
    put(&mut r, "b", term(&s.section, "fee"));
    land(&mut r, "a").unwrap();
    r.settle_tensions().unwrap();
    let t = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(change_sides(&t).len(), 1);
    let rx = r.subscribe();
    land(&mut r, "b").unwrap();
    r.settle_tensions().unwrap();
    let t2 = one(&r, TensionKind::DuplicateDefinition);
    assert!(change_sides(&t2).is_empty(), "every side is canon: {t2:#?}");
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    assert!(kinds.iter().any(|k| matches!(k,
        EventKind::TensionResolved { tension, rekeyed_to: Some(to), .. } if *tension == t.id && *to == t2.id)), "{kinds:?}");
    assert!(kinds.iter().any(|k| matches!(k,
        EventKind::TensionOpened { tension, rekeyed_from: Some(f), .. } if *tension == t2.id && *f == t.id)), "{kinds:?}");
    assert!(
        !kinds.iter().any(|k| matches!(k,
        EventKind::TensionResolved { tension, rekeyed_to: None, .. } if *tension == t.id)),
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
    r.settle_tensions().unwrap();
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
    r.settle_tensions().unwrap();
    assert!(
        of_kind(&r, TensionKind::PremiseShift).is_empty(),
        "no all-canon premise shift"
    );
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    assert!(
        kinds.iter().any(|k| matches!(k,
        EventKind::TensionResolved { tension, rekeyed_to: None, by: Some(by), .. }
            if *tension == t.id && by.starts_with("landing:"))),
        "{kinds:?}"
    );
    assert!(
        !kinds
            .iter()
            .any(|k| matches!(k, EventKind::TensionOpened { kind, .. } if kind == "premise_shift")),
        "{kinds:?}"
    );
}

#[test]
fn tevt1_a_dismissal_keeps_the_tension_and_settle_emits_nothing_for_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    r.settle_tensions().unwrap();
    let t = one(&r, TensionKind::EditCollision);
    let rev = Session::agent("agent:r".into(), Some("rr".into()), Role::Reviewer).unwrap();
    r.dismiss_tension(&rev, &tension_label(&t.id), "fine")
        .unwrap();
    assert_eq!(
        r.settle_tensions().unwrap(),
        0,
        "a dismissal is state, not a resolution"
    );
    assert!(r.read().tensions().unwrap().get(&t.id).is_some());
}

/// Final review M8: a pack reload changes the tension set without a store write; its events are
/// settled after the reload, with no `by` (no batch caused them).
#[test]
fn tevt1_a_pack_reload_settles_its_events_without_a_by() {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    r.settle_tensions().unwrap();
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
    assert!(r.settle_tensions().unwrap() > 0, "the reload's events");
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    assert!(
        kinds.iter().any(|k| matches!(k,
            EventKind::TensionResolved { tension, by, .. } if *tension == before.id && by.is_none())),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| matches!(k,
            EventKind::TensionOpened { severity, .. } if severity == "advisory")),
        "{kinds:?}"
    );
    // the next write's events carry its own `by` and nothing of the reload's
    let rx = r.subscribe();
    put(&mut r, "c", upd(&s.b, "B by c."));
    r.settle_tensions().unwrap();
    let kinds: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    assert!(
        !kinds.iter().any(
            |k| matches!(k, EventKind::TensionResolved { tension, .. } if *tension == before.id)
        ),
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

/// Final review M7: in the in-process engine (direct-mode CLI, the stdio MCP server) a settle
/// that panics never reaches the caller of a committed write, and the engine stays usable.
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
