//! D-Y32MN through the adapter: a full landing hash is a read scope when the snapshot's captured
//! refs reach it, from canon, a tag, a held chain, a replaced mark or a met landing's
//! `rebased_from`. A short landing id still matches among canon's chain only. Each landing of a
//! list has one place (D-H1SHC): in canon's chain, reached off it, or not stored.

use crate::hub_world::{
    AT, check, doc, land, moved, project, replayed, repo_of, seed, session, tag, world, write_in,
};
use rdy_adapter::history::LandingPlace;
use rdy_adapter::sync::CanonMove;
use rdy_adapter::{Hash, RdyError, ReadRequest, SearchRequest, Snapshot, StoreHandle};
use serde_json::{Value, json};
use std::collections::BTreeSet;

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

// ---------------------------------------------------------------------------------------------
// the place of each landing (D-H1SHC)

fn places(snap: &Snapshot, hashes: &[Hash]) -> Vec<(Hash, LandingPlace)> {
    snap.landing_places(hashes)
        .unwrap()
        .into_iter()
        .map(|f| (f.hash, f.place))
        .collect()
}

#[test]
fn cra3_places_tell_canon_held_replaced_and_missing() {
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
    drop((s1, c2));
    let s = h.snapshot().unwrap();
    let cq = check(&s, &[&w.eq]).unwrap();
    assert_eq!(
        h.move_canon(&cq, CanonMove::Hold, Some(w.p2)).unwrap(),
        moved(w.q2)
    );
    drop((s, cq));
    let none = Hash::of(b"none");
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(w.p2));
    assert_eq!(
        places(&snap, &[w.p2, w.q2, none]),
        vec![
            (w.p2, LandingPlace::InCanon),
            (w.q2, LandingPlace::OffCanon),
            (none, LandingPlace::NotStored)
        ]
    );
    // a stored object of another kind is not a landing
    let change = snap.landing_facts(w.p2).unwrap().record.changes[0];
    assert_eq!(
        places(&snap, &[change]),
        vec![(change, LandingPlace::NotStored)]
    );
    drop(snap);
    let s = h.snapshot().unwrap();
    let cf = check(&s, &[&w.eq]).unwrap();
    assert_eq!(
        h.move_canon(&cf, CanonMove::Force, Some(w.p2)).unwrap(),
        moved(w.q2)
    );
    drop((s, cf));
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(w.q2));
    assert_eq!(
        places(&snap, &[w.p2, w.q2]),
        vec![
            (w.p2, LandingPlace::OffCanon),
            (w.q2, LandingPlace::InCanon)
        ]
    );
}

#[test]
fn cra3_place_query_bounds_and_repeats() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    hold_q2(&h, &w);
    let snap = h.snapshot().unwrap();
    assert_eq!(
        places(&snap, &[w.q2, w.p1, w.q2]),
        vec![
            (w.q2, LandingPlace::OffCanon),
            (w.p1, LandingPlace::InCanon)
        ]
    );
    let many = vec![w.p1; 100_001];
    let e = snap.landing_places(&many).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_LIMIT",
            "landing place query exceeds 100000 hashes",
            "request at most 100000 landing hashes"
        )
    );
    assert_eq!(snap.landing_places(&many[..100_000]).unwrap().len(), 1);
}

#[test]
fn cra3_places_are_the_captured_refs() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    let s1 = h.snapshot().unwrap();
    hold_q2(&h, &w);
    assert_eq!(places(&s1, &[w.q2]), vec![(w.q2, LandingPlace::NotStored)]);
    let s2 = h.snapshot().unwrap();
    assert_eq!(places(&s2, &[w.q2]), vec![(w.q2, LandingPlace::OffCanon)]);
}

/// Redacts `target` in view `work` and lands the removal with a summary.
fn remove(r: &mut rdy_engine::Rdy, target: &str) {
    use rdy_engine::redaction::RedactInput;
    use rdy_model::{Provenance, Role};
    let s = session(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r
        .propose(&s, "work", "Remove the code", Some("test summary"))
        .unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(rdy_store::LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

#[test]
fn cra3_a_removal_since_the_snapshot_is_refused() {
    let _clock = rdy_engine::pin_clock(AT);
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(&dir.path().join("p"));
    let repo = repo_of(&r);
    let w = doc(&mut r, "work", "Intro", "Public words.");
    let secret = write_in(
        &mut r,
        "work",
        json!([{"op": "create", "type": "prose", "id": "$x", "parent": w.created["$s"], "body": "The launch code is ZEBRA-7731."}]),
    );
    let l1 = land(&mut r, "work");
    let v1 = tag(&mut r, "v1.0.0");
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), v1, &BTreeSet::new(), &mut out).unwrap();
    let t1 = dir.path().join("t1.rdysync");
    std::fs::write(&t1, out).unwrap();
    let h = StoreHandle::create_holder(&dir.path().join("hub"), repo).unwrap();
    let up = |p: &std::path::Path, tag: &str, landing: Hash| {
        h.take(
            &[std::fs::File::open(p).unwrap()],
            rdy_adapter::take::UploadTarget {
                tag: tag.into(),
                landing,
            },
        )
        .unwrap();
    };
    up(&t1, "v1.0.0", l1);
    let s1 = h.snapshot().unwrap();
    assert_eq!(places(&s1, &[l1]), vec![(l1, LandingPlace::OffCanon)]);

    // the project removes the secret; the holder takes the removal with the next tag
    let target = secret.created["$x"].clone();
    remove(&mut r, &target);
    let v2 = tag(&mut r, "v2.0.0");
    let l2 = r.store().read_tag(&v2).unwrap().landing;
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), v2, &BTreeSet::new(), &mut out).unwrap();
    let t2 = dir.path().join("t2.rdysync");
    std::fs::write(&t2, out).unwrap();
    r.close().unwrap();
    up(&t2, "v2.0.0", l2);

    let e = s1.landing_places(&[l1]).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e:#?}");
    drop(s1);
    let s2 = h.snapshot().unwrap();
    assert_eq!(
        places(&s2, &[l1, l2]),
        vec![(l1, LandingPlace::OffCanon), (l2, LandingPlace::OffCanon)]
    );
    h.close().unwrap();
}
