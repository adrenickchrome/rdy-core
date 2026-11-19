//! The reads a hub needs to serve the sync steps, through the adapter alone: a page of a
//! history's keys after a given key, one batch of named keys, and the store's features. Core's
//! `write_history` is the oracle: a page's keys and a named-keys batch are exactly what it would
//! export for the same root and base. Stores are built through real writes and landings, with
//! tags (and their answers), a signature and a view, so the closures hold attachments.

use rdy_adapter::stream::{BatchReport, KeyPage, ObjectKey, StreamPurpose, check_batch};
use rdy_adapter::sync::{CanonMove, Moved};
use rdy_adapter::{
    FeatureClass, Hash, RdyError, RepoId, Snapshot, StoreFeature, StoreFeatures, StoreHandle,
};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::{Provenance, Role};
use rdy_store::kinds;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The writer's clock of every move.
const AT: i64 = 1_763_164_800_000;

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn doc(r: &mut Rdy, view: &str, title: &str, body: &str) {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"title": title, "doc_type": "page"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": body}
            ]))
            .unwrap(),
            justification: None,
            message: "scc8r".into(),
        },
    )
    .unwrap();
}

fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&session(Role::Writer), view, view, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap()
}

fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn view_head_of(r: &Rdy, name: &str) -> Hash {
    r.store().reader().view_head(name).unwrap().unwrap().head
}

/// A project with two canon landings (each tagged; the first signed) and an open view `draft`
/// of two ops. Closed again.
struct World {
    dir: tempfile::TempDir,
    repo: RepoId,
    p1: Hash,
    p2: Hash,
    sig1: Hash,
    d_root: Hash,
    d_head: Hash,
}

impl World {
    fn path(&self) -> PathBuf {
        self.dir.path().join("p")
    }
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    std::fs::create_dir_all(&pd).unwrap();
    drop(Rdy::init(&pd, "rdy-standard").unwrap());
    let mut r = Rdy::open(&pd).unwrap();
    let repo = r.store().reader().repo_id().unwrap().unwrap();
    doc(&mut r, "w0", "Canon", "The hub keeps one canon.");
    let p1 = land(&mut r, "w0");
    tag(&mut r, "v1.0.0");
    let sig1 = r
        .store_mut()
        .put_signature(&rdy_adapter::history::Signature {
            subject: p1,
            namespace: "rdy-approval-v1".into(),
            statement: b"x".to_vec(),
            sig: b"not checked here".to_vec(),
            signed_by: Provenance {
                actor: "agent:s".into(),
                run: None,
                role: Role::Approver,
                at_ms: 7,
            },
        })
        .unwrap();
    r.seal_open().unwrap();
    doc(&mut r, "draft", "Draft", "Work in flight.");
    doc(&mut r, "draft", "Draft two", "More work in flight.");
    let d_head = view_head_of(&r, "draft");
    let mut d_root = d_head;
    while let Some(prev) = r.store().reader().view_op(&d_root).unwrap().prev {
        d_root = prev;
    }
    doc(&mut r, "w1", "Extend", "The canon moves forward.");
    let p2 = land(&mut r, "w1");
    tag(&mut r, "v2.0.0");
    r.close().unwrap();
    World {
        dir,
        repo,
        p1,
        p2,
        sig1,
        d_root,
        d_head,
    }
}

const CANON: StreamPurpose = StreamPurpose::CanonHistory;
const VIEW: StreamPurpose = StreamPurpose::ViewHistory;

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

/// What core's own `write_history` exports for the root and base: the oracle.
fn oracle(
    snap: &Snapshot,
    purpose: StreamPurpose,
    root: Hash,
    base: Option<Hash>,
) -> (Vec<u8>, BatchReport) {
    let mut out = Vec::new();
    let report = snap
        .write_history(purpose, root, base, &BTreeSet::new(), &mut out)
        .unwrap();
    (out, report)
}

/// Every page of `limit` keys, from the start, following each `next`.
fn pages(
    snap: &Snapshot,
    purpose: StreamPurpose,
    root: Hash,
    base: Option<Hash>,
    limit: usize,
) -> Vec<KeyPage> {
    let mut all = Vec::new();
    let mut after = None;
    loop {
        let page = snap
            .history_page(purpose, root, base, after, limit)
            .unwrap();
        after = page.next;
        all.push(page);
        if after.is_none() {
            return all;
        }
    }
}

fn code(e: RdyError) -> String {
    e.code.to_string()
}

#[test]
fn scc8r_history_page_follows_write_history_order_after_a_key() {
    let w = world();
    let h = StoreHandle::open(&w.path()).unwrap();
    let snap = h.snapshot().unwrap();
    for (purpose, root, base) in [
        (CANON, w.p2, None),
        (CANON, w.p2, Some(w.p1)),
        (CANON, w.p1, None),
        (VIEW, w.d_head, None),
        (VIEW, w.d_head, Some(w.d_root)),
    ] {
        let (_, report) = oracle(&snap, purpose, root, base);
        assert!(report.keys.len() > 3, "the history is long enough to page");
        for limit in [1, 2, 100_000] {
            let ps = pages(&snap, purpose, root, base, limit);
            let joined: Vec<ObjectKey> = ps.iter().flat_map(|p| p.keys.clone()).collect();
            assert_eq!(
                joined, report.keys,
                "{purpose:?} {root} {base:?} limit {limit}"
            );
            let last = ps.len() - 1;
            for (i, p) in ps.iter().enumerate() {
                assert!(p.keys.len() <= limit && !p.keys.is_empty());
                if i == last {
                    assert_eq!(p.next, None, "the last page ends with a null next");
                } else {
                    assert_eq!(p.keys.len(), limit);
                    assert_eq!(p.next, p.keys.last().copied());
                }
            }
        }
        // `after` the last key is an empty page at the end
        let end = report.keys.last().copied();
        let page = snap.history_page(purpose, root, base, end, 5).unwrap();
        assert_eq!((page.keys.len(), page.next), (0, None));
        // `after` a middle key starts at the next key
        let mid = report.keys[1];
        let page = snap
            .history_page(purpose, root, base, Some(mid), 2)
            .unwrap();
        assert_eq!(page.keys, report.keys[2..4].to_vec());
    }
    drop(snap);
    h.close().unwrap();
}

#[test]
fn scc8r_history_page_base_the_store_lacks_counts_as_none() {
    let w = world();
    let h = StoreHandle::open(&w.path()).unwrap();
    let snap = h.snapshot().unwrap();
    let none = pages(&snap, CANON, w.p2, None, 100_000);
    let missing = Hash::of(b"a landing no store holds");
    assert_eq!(pages(&snap, CANON, w.p2, Some(missing), 100_000), none);
    // a held base removes its link closure but keeps later attachments of its records
    let held = pages(&snap, CANON, w.p2, Some(w.p1), 100_000);
    let (_, report) = oracle(&snap, CANON, w.p2, Some(w.p1));
    assert_eq!(held[0].keys, report.keys);
    let all = &none[0].keys;
    assert!(held[0].keys.len() < all.len());
    assert!(held[0].keys.iter().all(|k| all.contains(k)));
    assert!(!held[0].keys.contains(&key(kinds::LANDING, w.p1)));
    assert!(held[0].keys.contains(&key(kinds::LANDING, w.p2)));
    assert!(
        held[0].keys.contains(&key(kinds::SIGNATURE, w.sig1)),
        "a later signature of a base record still travels"
    );
    drop(snap);
    h.close().unwrap();
}

/// A holder that has taken nothing, and the batch files of `w`'s two canon pushes.
fn export_to(snap: &Snapshot, purpose: StreamPurpose, root: Hash, base: Option<Hash>, to: &Path) {
    let file = File::create(to).unwrap();
    snap.write_history(purpose, root, base, &BTreeSet::new(), file)
        .unwrap();
}

#[test]
fn scc8r_history_page_refusals_use_core_errors() {
    let w = world();
    let h = StoreHandle::open(&w.path()).unwrap();
    let snap = h.snapshot().unwrap();
    let unheld = Hash::of(b"no such root");
    let e = snap
        .history_page(CANON, unheld, None, None, 10)
        .unwrap_err();
    assert_eq!(code(e), "E_UNRESOLVED_REF");
    let e = snap
        .history_page(StreamPurpose::TagHistory, w.p2, None, None, 10)
        .unwrap_err();
    assert_eq!(code(e), "E_STREAM_PURPOSE");
    for limit in [0usize, 100_001] {
        let e = snap
            .history_page(CANON, w.p2, None, None, limit)
            .unwrap_err();
        assert_eq!(code(e), "E_LIMIT", "limit {limit}");
    }
    // an `after` outside the closure: a key the store lacks, and one it holds elsewhere
    let lacking = key(kinds::LANDING, unheld);
    let elsewhere = key(kinds::VIEW_OP, w.d_head);
    for after in [lacking, elsewhere] {
        let e = snap
            .history_page(CANON, w.p2, None, Some(after), 10)
            .unwrap_err();
        assert_eq!(code(e), "E_UNRESOLVED_REF", "{after:?}");
    }
    drop(snap);
    h.close().unwrap();

    // nothing is read past the snapshot's log end: a landing moved in after it is not there
    let hub = tempfile::tempdir().unwrap();
    let source = StoreHandle::open(&w.path()).unwrap();
    let s = source.snapshot().unwrap();
    let e1 = w.dir.path().join("e1.rdysync");
    let e2 = w.dir.path().join("e2.rdysync");
    export_to(&s, CANON, w.p1, None, &e1);
    export_to(&s, CANON, w.p2, Some(w.p1), &e2);
    drop(s);
    source.close().unwrap();
    let holder = StoreHandle::create_holder(hub.path(), w.repo).unwrap();
    let _clock = rdy_engine::pin_clock(AT);
    let c1 = holder.snapshot().unwrap();
    let q = c1.check_quarantine(&[File::open(&e1).unwrap()]).unwrap();
    holder.move_canon(&q, CanonMove::Seed, None).unwrap();
    let before = holder.snapshot().unwrap();
    let q = before
        .check_quarantine(&[File::open(&e2).unwrap()])
        .unwrap();
    assert_eq!(
        holder
            .move_canon(&q, CanonMove::Extend, Some(w.p1))
            .unwrap(),
        Moved::Moved { tip: w.p2 }
    );
    let after = holder.snapshot().unwrap();
    assert!(after.history_page(CANON, w.p2, None, None, 10).is_ok());
    let e = before
        .history_page(CANON, w.p2, None, None, 10)
        .unwrap_err();
    assert_eq!(code(e), "E_UNRESOLVED_REF");
    assert!(before.history_page(CANON, w.p1, None, None, 100).is_ok());
    drop((c1, q, before, after));
    holder.close().unwrap();
}

fn root_key(purpose: StreamPurpose, root: Hash) -> ObjectKey {
    match purpose {
        VIEW => key(kinds::VIEW_OP, root),
        _ => key(kinds::LANDING, root),
    }
}

#[test]
fn scc8r_named_keys_batch_has_its_purpose_root_and_only_those_records() {
    let w = world();
    let h = StoreHandle::open(&w.path()).unwrap();
    let snap = h.snapshot().unwrap();
    for (purpose, root) in [(CANON, w.p2), (VIEW, w.d_head)] {
        let (whole_bytes, whole) = oracle(&snap, purpose, root, None);
        let mut saw_root_prerequisite = false;
        for p in pages(&snap, purpose, root, None, 3) {
            let mut out = Vec::new();
            let report = snap
                .write_history_keys(purpose, root, &p.keys, &mut out)
                .unwrap();
            // the batch passes the check against the store's repo
            let checked = check_batch(out.as_slice(), Some(w.repo)).unwrap();
            assert_eq!(checked.keys, report.keys);
            assert_eq!(checked.header.purpose, purpose);
            assert_eq!(checked.header.tag, root);
            assert_eq!(report.header, checked.header);
            // exactly the named records, deltas (kind 30) included, in the order core writes
            // that set
            let named: BTreeSet<ObjectKey> = p.keys.iter().copied().collect();
            let written: BTreeSet<ObjectKey> = report.keys.iter().copied().collect();
            assert_eq!(written, named);
            let known: BTreeSet<ObjectKey> = whole
                .keys
                .iter()
                .copied()
                .filter(|k| !named.contains(k))
                .collect();
            let mut by_core = Vec::new();
            let core = snap
                .write_history(purpose, root, None, &known, &mut by_core)
                .unwrap();
            // core's export of the set cannot reach the delta of a landing it leaves out, so
            // it is the oracle for the other records, in order
            let without = |ks: &[ObjectKey]| -> Vec<ObjectKey> {
                ks.iter()
                    .copied()
                    .filter(|k| k.kind != kinds::SNAPSHOT_DELTA)
                    .collect()
            };
            assert_eq!(without(&report.keys), without(&core.keys));
            // every batch lists the features of the whole history, so batches of one get share
            // one header; core's export of the same set lists only what it needs
            assert_eq!(report.header.features, whole.header.features);
            // a delta of a landing the batch omits adds that landing (or its base delta) as a
            // prerequisite; check_batch has refused any prerequisite no record needs
            assert!(
                core.header
                    .prerequisites
                    .iter()
                    .all(|k| report.header.prerequisites.contains(k))
            );
            if !report.keys.iter().any(|k| k.kind == kinds::SNAPSHOT_DELTA) {
                assert_eq!(report.header.prerequisites, core.header.prerequisites);
            }
            // the root is a prerequisite exactly when it is not a record
            let rk = root_key(purpose, root);
            assert_eq!(
                report.header.prerequisites.contains(&rk),
                !report.keys.contains(&rk)
            );
            saw_root_prerequisite |= !report.keys.contains(&rk);
            // a record's missing needs are prerequisites, never records of the batch
            assert!(
                report
                    .header
                    .prerequisites
                    .iter()
                    .all(|k| !report.keys.contains(k))
            );
        }
        assert!(
            saw_root_prerequisite,
            "some page leaves the root to a later batch"
        );
        // all keys at once is the whole export, byte for byte
        let mut all = Vec::new();
        snap.write_history_keys(purpose, root, &whole.keys, &mut all)
            .unwrap();
        assert_eq!(all, whole_bytes);
        // input order and repeats do not matter
        let mut shuffled = whole.keys.clone();
        shuffled.reverse();
        shuffled.extend(whole.keys.iter().copied());
        let mut again = Vec::new();
        snap.write_history_keys(purpose, root, &shuffled, &mut again)
            .unwrap();
        assert_eq!(again, whole_bytes);
    }
    drop(snap);
    h.close().unwrap();
}

/// Writes `bytes` to a file in `dir`.
fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn scc8r_named_keys_split_landings_and_deltas_into_separate_batches() {
    let w = world();
    let source = StoreHandle::open(&w.path()).unwrap();
    let s = source.snapshot().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (whole_bytes, whole) = oracle(&s, CANON, w.p2, None);
    let one = put(scratch.path(), "whole.rdysync", &whole_bytes);
    let is_delta = |k: &ObjectKey| k.kind == kinds::SNAPSHOT_DELTA;
    let deltas: Vec<ObjectKey> = whole.keys.iter().copied().filter(is_delta).collect();
    let rest: Vec<ObjectKey> = whole
        .keys
        .iter()
        .copied()
        .filter(|k| !is_delta(k))
        .collect();
    assert!(deltas.len() >= 2, "each landing has its delta");
    // a request of deltas alone gives exactly those records, with no landing
    let mut d_out = Vec::new();
    let d = s
        .write_history_keys(CANON, w.p2, &deltas, &mut d_out)
        .unwrap();
    assert_eq!(
        d.keys.iter().copied().collect::<BTreeSet<_>>(),
        deltas.iter().copied().collect()
    );
    assert!(d.keys.iter().all(is_delta));
    let checked = check_batch(d_out.as_slice(), Some(w.repo)).unwrap();
    assert_eq!(checked.keys, d.keys);
    // a request of landings and everything else but the deltas gives no delta
    let mut r_out = Vec::new();
    let r = s
        .write_history_keys(CANON, w.p2, &rest, &mut r_out)
        .unwrap();
    assert_eq!(
        r.keys.iter().copied().collect::<BTreeSet<_>>(),
        rest.iter().copied().collect()
    );
    assert!(r.keys.iter().any(|k| k.kind == kinds::LANDING));
    assert!(!r.keys.iter().any(is_delta));
    check_batch(r_out.as_slice(), Some(w.repo)).unwrap();
    // the two batches share one header shape (repo, purpose, root, features)
    assert_eq!(d.header.features, r.header.features);
    assert_eq!(d.header.tag, r.header.tag);
    // a single landing and a single delta, each alone
    let l0 = *rest.iter().find(|k| k.kind == kinds::LANDING).unwrap();
    let mut l_out = Vec::new();
    let l = s
        .write_history_keys(CANON, w.p2, &[l0], &mut l_out)
        .unwrap();
    assert_eq!(l.keys, vec![l0]);
    let mut x_out = Vec::new();
    let x = s
        .write_history_keys(CANON, w.p2, &[deltas[0]], &mut x_out)
        .unwrap();
    assert_eq!(x.keys, vec![deltas[0]]);
    check_batch(x_out.as_slice(), Some(w.repo)).unwrap();
    let d_file = put(scratch.path(), "deltas.rdysync", &d_out);
    let r_file = put(scratch.path(), "rest.rdysync", &r_out);
    drop(s);
    source.close().unwrap();

    // taken together (delta batch first, then the rest), they give what one export gives
    let _clock = rdy_engine::pin_clock(AT);
    let take = |name: &str, files: &[&PathBuf]| {
        let h = StoreHandle::create_holder(&scratch.path().join(name), w.repo).unwrap();
        let before = h.snapshot().unwrap();
        let handles: Vec<File> = files.iter().map(|p| File::open(p).unwrap()).collect();
        let q = before.check_quarantine(&handles).unwrap();
        let facts = q.facts().clone();
        assert_eq!(
            h.move_canon(&q, CanonMove::Seed, None).unwrap(),
            Moved::Moved { tip: w.p2 }
        );
        drop((before, q));
        h.close().unwrap();
        facts
    };
    let a = take("whole", &[&one]);
    let b = take("split", &[&d_file, &r_file]);
    assert_eq!(a.objects, b.objects);
    assert_eq!(a.min_rdy_after, b.min_rdy_after);
}

#[test]
fn scc8r_named_keys_batches_rebuild_the_whole_history() {
    let w = world();
    let source = StoreHandle::open(&w.path()).unwrap();
    let s = source.snapshot().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (whole_bytes, whole) = oracle(&s, CANON, w.p2, None);
    let one = put(scratch.path(), "whole.rdysync", &whole_bytes);
    let mut paged = Vec::new();
    for (i, p) in pages(&s, CANON, w.p2, None, 3).iter().enumerate() {
        let mut out = Vec::new();
        s.write_history_keys(CANON, w.p2, &p.keys, &mut out)
            .unwrap();
        paged.push(put(scratch.path(), &format!("page{i}.rdysync"), &out));
    }
    assert!(paged.len() > 2);
    let source_pages = pages(&s, CANON, w.p2, None, 100_000);
    drop(s);
    source.close().unwrap();

    let _clock = rdy_engine::pin_clock(AT);
    let take = |name: &str, files: &[&PathBuf]| {
        let h = StoreHandle::create_holder(&scratch.path().join(name), w.repo).unwrap();
        let before = h.snapshot().unwrap();
        let handles: Vec<File> = files.iter().map(|p| File::open(p).unwrap()).collect();
        let q = before.check_quarantine(&handles).unwrap();
        let facts = q.facts().clone();
        assert_eq!(
            h.move_canon(&q, CanonMove::Seed, None).unwrap(),
            Moved::Moved { tip: w.p2 }
        );
        let after = h.snapshot().unwrap();
        let keys = pages(&after, CANON, w.p2, None, 100_000);
        let features = after.features().clone();
        let min = after.min_rdy();
        drop((before, q, after));
        h.close().unwrap();
        (facts, keys, features, min)
    };
    let a = take("whole", &[&one]);
    let refs: Vec<&PathBuf> = paged.iter().collect();
    let b = take("paged", &refs);
    // the same records (with their snapshot deltas), the same pages, features and minimum
    assert_eq!(a.0.objects, b.0.objects);
    assert!(
        a.0.objects.iter().any(|k| k.kind == kinds::SNAPSHOT_DELTA),
        "the deltas of kind 30 travel"
    );
    assert_eq!(a.0.min_rdy_after, b.0.min_rdy_after);
    assert_eq!(a.1, b.1);
    assert_eq!(a.1, source_pages);
    assert_eq!(a.1[0].keys, whole.keys);
    assert_eq!(a.2, b.2);
    assert_eq!(a.3, b.3);
}

#[test]
fn scc8r_named_keys_batch_refuses_keys_outside_the_closure() {
    let w = world();
    let h = StoreHandle::open(&w.path()).unwrap();
    let snap = h.snapshot().unwrap();
    // the store holds p2 and the draft view op, but p1's history does not
    let outside = [
        key(kinds::LANDING, w.p2),
        key(kinds::VIEW_OP, w.d_head),
        key(kinds::LANDING, Hash::of(b"the store lacks this")),
    ];
    let inside = snap.history_page(CANON, w.p1, None, None, 2).unwrap().keys;
    for bad in outside {
        let mut keys = inside.clone();
        keys.push(bad);
        let mut out = Vec::new();
        let e = snap
            .write_history_keys(CANON, w.p1, &keys, &mut out)
            .unwrap_err();
        assert_eq!(code(e), "E_UNRESOLVED_REF", "{bad:?}");
        assert!(out.is_empty(), "nothing is written for {bad:?}");
    }
    // root not held, and another purpose
    let mut out = Vec::new();
    let e = snap
        .write_history_keys(CANON, Hash::of(b"no root"), &inside, &mut out)
        .unwrap_err();
    assert_eq!(code(e), "E_UNRESOLVED_REF");
    let e = snap
        .write_history_keys(StreamPurpose::TagHistory, w.p1, &inside, &mut out)
        .unwrap_err();
    assert_eq!(code(e), "E_STREAM_PURPOSE");
    assert!(out.is_empty());
    // over the key limit: 100001 distinct keys; a repeated key counts once
    let many: Vec<ObjectKey> = (0..=100_000u32)
        .map(|i| key(kinds::LANDING, Hash::of(&i.to_be_bytes())))
        .collect();
    let e = snap
        .write_history_keys(CANON, w.p1, &many, &mut out)
        .unwrap_err();
    assert_eq!(code(e), "E_LIMIT");
    assert!(out.is_empty());
    let repeated = vec![inside[0]; 100_001];
    snap.write_history_keys(CANON, w.p1, &repeated, &mut out)
        .unwrap();
    assert!(!out.is_empty());
    check_batch(out.as_slice(), Some(w.repo)).unwrap();
    drop(snap);
    h.close().unwrap();
}

/// A consumer names the feature types from `rdy_adapter` alone.
fn _types_are_named(f: &StoreFeatures) -> Vec<(&str, FeatureClass)> {
    f.features
        .iter()
        .map(|x: &StoreFeature| (x.name.as_str(), x.class))
        .collect()
}

#[test]
fn scc8r_store_features_read_is_public() {
    let w = world();
    let _clock = rdy_engine::pin_clock(AT);
    // a project's snapshot gives the manifest as the store holds it
    let h = StoreHandle::open(&w.path()).unwrap();
    let snap = h.snapshot().unwrap();
    let names = _types_are_named(snap.features());
    assert!(!names.is_empty());
    drop(snap);
    h.close().unwrap();
    let raw = Rdy::open(&w.path()).unwrap();
    let manifest = raw.store().reader().store_features().unwrap();
    raw.close().unwrap();
    let h = StoreHandle::open(&w.path()).unwrap();
    assert_eq!(h.snapshot().unwrap().features(), &manifest);
    h.close().unwrap();

    // a feature activated after the snapshot is not in it: the first canon move of a holder
    // turns `hub_canon` on
    let scratch = tempfile::tempdir().unwrap();
    let source = StoreHandle::open(&w.path()).unwrap();
    let s = source.snapshot().unwrap();
    let e1 = scratch.path().join("e1.rdysync");
    export_to(&s, CANON, w.p1, None, &e1);
    drop(s);
    source.close().unwrap();
    let holder = StoreHandle::create_holder(&scratch.path().join("hub"), w.repo).unwrap();
    let before = holder.snapshot().unwrap();
    let has = |s: &Snapshot| s.features().features.iter().any(|f| f.name == "hub_canon");
    assert!(!has(&before));
    let q = before
        .check_quarantine(&[File::open(&e1).unwrap()])
        .unwrap();
    holder.move_canon(&q, CanonMove::Seed, None).unwrap();
    let after = holder.snapshot().unwrap();
    assert!(has(&after));
    assert!(!has(&before), "the earlier snapshot keeps what it captured");
    drop((before, q, after));
    holder.close().unwrap();
    let raw = Rdy::open_holder(&scratch.path().join("hub")).unwrap();
    let stored = raw.store().reader().store_features().unwrap();
    raw.close().unwrap();
    let holder = StoreHandle::open_holder(&scratch.path().join("hub")).unwrap();
    assert_eq!(holder.snapshot().unwrap().features(), &stored);
    holder.close().unwrap();
}
