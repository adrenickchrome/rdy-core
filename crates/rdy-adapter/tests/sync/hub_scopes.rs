//! D-Y32MN through the adapter: a full landing hash is a read scope when the snapshot's captured
//! refs reach it, from canon, a tag, a held chain, a replaced mark or a met landing's
//! `rebased_from`. A short landing id still matches among canon's chain only.

use crate::hub_world::{AT, check, doc, land, moved, project, replayed, seed, world};
use rdy_adapter::sync::CanonMove;
use rdy_adapter::{Hash, RdyError, ReadRequest, SearchRequest, Snapshot, StoreHandle};
use serde_json::Value;

fn search(snap: &Snapshot, query: &str, scope: &str) -> Result<Value, RdyError> {
    snap.search(SearchRequest {
        query: query.into(),
        ignore_case: true,
        scope: Some(scope.into()),
        ..Default::default()
    })
}

fn landing(h: &Hash) -> String {
    format!("landing:{h}")
}

/// The first match of a search reply: its block id and its path.
fn first_match(reply: &Value) -> (String, String) {
    let m = &reply["matches"][0];
    (
        m["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{reply:#}"))
            .into(),
        m["path"].as_str().unwrap_or_default().into(),
    )
}

/// Seeds `p1` and holds `q2` beside it.
fn hold_q2(h: &StoreHandle, w: &crate::hub_world::World) {
    let s = h.snapshot().unwrap();
    let c = check(&s, &[&w.eq]).unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Hold, Some(w.p1)).unwrap(),
        moved(w.q2)
    );
}

#[test]
fn cra1_full_landing_hash_reads_a_held_landing() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    hold_q2(&h, &w);
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(w.p1));
    let reply = search(&snap, "beside canon", &landing(&w.q2)).unwrap();
    let (id, path) = first_match(&reply);
    assert!(path == "held", "{reply:#}");
    let read = snap
        .read(ReadRequest {
            id,
            scope: Some(landing(&w.q2)),
            ..Default::default()
        })
        .unwrap();
    assert!(
        read.to_string()
            .contains("A chain the hub holds beside canon."),
        "{read:#}"
    );
}

#[test]
fn cra1_full_landing_hash_reads_a_replaced_landing() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    let s1 = h.snapshot().unwrap();
    let c2 = check(&s1, &[&w.e2]).unwrap();
    assert_eq!(
        h.move_canon(&c2, CanonMove::Extend, Some(w.p1)).unwrap(),
        moved(w.p2)
    );
    let s2 = h.snapshot().unwrap();
    let cq = check(&s2, &[&w.eq]).unwrap();
    assert_eq!(
        h.move_canon(&cq, CanonMove::Force, Some(w.p2)).unwrap(),
        moved(w.q2)
    );
    drop((s1, c2, s2, cq));
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(w.q2));
    let reply = search(&snap, "canon moves forward", &landing(&w.p2)).unwrap();
    let (_, path) = first_match(&reply);
    assert!(path == "extend", "{reply:#}");
}

#[test]
fn cra1_rebased_from_reaches_its_source() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = replayed();
    let snap = w.hub.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(w.l2));
    assert!(snap.tags().unwrap().is_empty());
    assert!(
        !snap.canon_history().unwrap().iter().any(|f| f.hash == w.l),
        "canon's chain does not hold the replay source"
    );
    let reply = search(&snap, "hub has not seen", &landing(&w.l)).unwrap();
    let (_, path) = first_match(&reply);
    assert!(path == "local", "{reply:#}");
    assert_ne!(w.p2, w.l);
}

#[test]
fn cra1_short_landing_id_never_matches_a_held_chain() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    hold_q2(&h, &w);
    let snap = h.snapshot().unwrap();
    let short = format!("landing:{}", &w.q2.to_string()[..12]);
    let e = search(&snap, "beside canon", &short).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:#?}");
    let e = snap
        .read(ReadRequest {
            id: "x".into(),
            scope: Some(short),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:#?}");
}

#[test]
fn cra1_unreached_hash_is_not_found() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    let snap = h.snapshot().unwrap();
    let none = Hash::of(b"none");
    let want =
        format!("landing {none} is not reached from canon, a tag, a held chain or a replaced mark");
    let e = search(&snap, "canon", &landing(&none)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_NOT_FOUND",
            want.as_str(),
            "name a landing this store reaches, or read canon"
        )
    );
    let e = snap
        .read(ReadRequest {
            id: "x".into(),
            scope: Some(landing(&none)),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!((e.code, e.message), ("E_NOT_FOUND", want));
}

#[test]
fn cra1_reach_is_the_captured_refs() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    let s1 = h.snapshot().unwrap();
    hold_q2(&h, &w);
    let e = search(&s1, "beside canon", &landing(&w.q2)).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:#?}");
    let s2 = h.snapshot().unwrap();
    let reply = search(&s2, "beside canon", &landing(&w.q2)).unwrap();
    let (_, path) = first_match(&reply);
    assert!(path == "held", "{reply:#}");
}

#[test]
fn cra1_project_full_hash_equals_short_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project(dir.path());
    doc(&mut p, "w0", "Canon", "The project keeps one canon.");
    let l1 = land(&mut p, "w0");
    doc(&mut p, "w1", "Next", "The canon moves forward.");
    land(&mut p, "w1");
    p.close().unwrap();
    let h = StoreHandle::open(dir.path()).unwrap();
    let snap = h.snapshot().unwrap();
    let full = search(&snap, "canon", &landing(&l1)).unwrap();
    let short = search(
        &snap,
        "canon",
        &format!("landing:{}", &l1.to_string()[..12]),
    )
    .unwrap();
    assert_eq!(full, short);
    assert!(!full["matches"].as_array().unwrap().is_empty(), "{full:#}");
}
