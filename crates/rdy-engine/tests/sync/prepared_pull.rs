//! r59012: one captured history for a first pull. A `PreparedPull` walks the whole history once
//! (one closure, one key set); the known pages and every get of that pull read it and walk
//! nothing again. The old selection of `write_history_keys` (a whole walk, then a second walk
//! with the unselected keys as known) is kept here as the oracle: each batch must equal its
//! bytes, order, prerequisites and refusals.
use rdy_engine::history_facts::MAX_KEYS;
use rdy_engine::redaction::RedactInput;
use rdy_engine::stream_error::to_rdy;
use rdy_engine::stream_export::{self, PreparedPull};
use rdy_engine::*;
use rdy_model::signature::Signature;
use rdy_model::*;
use rdy_store::LandRequest;
use rdy_store::closure::{
    HistoryClosure, StoreSource, history_closure_before, history_closure_named_before, write_batch,
};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_stream::{StreamHeader, StreamPurpose, check_batch};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

const CANON: StreamPurpose = StreamPurpose::CanonHistory;
const VIEW: StreamPurpose = StreamPurpose::ViewHistory;
const CANARY: &str = "ZEBRA-7731";

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

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "r59012".into(),
        },
    )
    .unwrap()
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

fn sign(r: &mut Rdy, subject: Hash, n: u8) -> Hash {
    r.store_mut()
        .put_signature(&Signature {
            subject,
            namespace: "rdy-approval-v1".into(),
            statement: vec![n],
            sig: b"not checked here".to_vec(),
            signed_by: Provenance {
                actor: "agent:s".into(),
                run: None,
                role: Role::Approver,
                at_ms: 7,
            },
        })
        .unwrap()
}

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: String) {
    let s = session(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
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

struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    p1: Hash,
    p2: Hash,
    secret: String,
    draft_head: Hash,
}

/// Two canon landings (each tagged, the first signed), a repo link (a record that needs a
/// feature), a secret prose block, and an open view `draft` of two ops.
fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$s", "parent": "$d", "body": format!("The launch code is {CANARY}.")},
            {"op": "create", "type": "prose", "id": "$q", "parent": "$d", "body": "A plain second block."},
            {"op": "create", "type": "repo_link", "id": "$l", "parent": "$d",
             "fields": {"alias": "core", "repo": RepoId([7; 16]).to_string(), "profile": "pf-AGNT1"}}
        ]),
    );
    let d = w.created["$d"].clone();
    let secret = w.created["$s"].clone();
    let p1 = land(&mut r, "work");
    tag(&mut r, "v1.0.0");
    sign(&mut r, p1, 1);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": d, "fields": {"title": "A2"}}]),
    );
    let p2 = land(&mut r, "work");
    tag(&mut r, "v2.0.0");
    r.seal_open().unwrap();
    write(
        &mut r,
        "draft",
        json!([{"op": "create", "type": "doc", "id": "$e", "fields": {"name": "e", "title": "E", "doc_type": "page"}}]),
    );
    write(
        &mut r,
        "draft",
        json!([{"op": "create", "type": "doc", "id": "$f", "fields": {"name": "f", "title": "F", "doc_type": "page"}}]),
    );
    let draft_head = r.store().reader().view_head("draft").unwrap().unwrap().head;
    World {
        _dir: dir,
        r,
        p1,
        p2,
        secret,
        draft_head,
    }
}

// ---------------------------------------------------------------------------------------------
// the old selection, kept as the oracle

fn limit_error() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("object presence query exceeds {MAX_KEYS} keys"),
        format!("request at most {MAX_KEYS} object keys"),
    )
}

/// What the whole walk of `root` holds: the old `history_page` and `write_history_keys` start.
fn old_whole(
    read: &ReadState,
    purpose: StreamPurpose,
    root: Hash,
) -> Result<HistoryClosure, RdyError> {
    history_closure_before(
        read.reader(),
        purpose,
        root,
        &BTreeSet::new(),
        None,
        read.seq(),
    )
    .map_err(to_rdy)
}

/// The old `write_history_keys`, as it stood before the prepared pull: two walks and one batch.
fn old_keys(
    read: &ReadState,
    purpose: StreamPurpose,
    root: Hash,
    keys: &[ObjectKey],
) -> Result<Vec<u8>, RdyError> {
    let wanted: BTreeSet<ObjectKey> = keys.iter().copied().collect();
    if wanted.len() > MAX_KEYS {
        return Err(limit_error());
    }
    read.ensure_current_epoch()?;
    let reader = read.reader();
    let end = read.seq();
    let whole = history_closure_before(reader, purpose, root, &BTreeSet::new(), None, end)
        .map_err(to_rdy)?;
    let held: BTreeSet<ObjectKey> = whole.records.iter().copied().collect();
    if let Some(k) = wanted.iter().find(|k| !held.contains(k)) {
        return Err(to_rdy(StreamError::unresolved(0, 0, *k)));
    }
    let known: BTreeSet<ObjectKey> = held.difference(&wanted).copied().collect();
    let deltas: Vec<ObjectKey> = wanted
        .iter()
        .copied()
        .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
        .collect();
    let mut found = history_closure_named_before(reader, purpose, root, &known, &deltas, end)
        .map_err(to_rdy)?;
    found.features = whole.features;
    let emitted: BTreeSet<ObjectKey> = found.records.iter().copied().collect();
    assert_eq!(emitted, wanted, "the oracle holds exactly the named keys");
    let repo_id = reader.repo_id().unwrap().unwrap();
    let header = match purpose {
        StreamPurpose::ViewHistory => {
            StreamHeader::view_history(found.features, repo_id, root, found.prerequisites)
        }
        _ => StreamHeader::canon_history(found.features, repo_id, root, found.prerequisites),
    };
    let mut source = StoreSource { reader, end };
    let mut out = Vec::new();
    write_batch(&mut source, &header, &found.records, &mut out).map_err(to_rdy)?;
    read.ensure_current_epoch()?;
    Ok(out)
}

fn same_error(a: &RdyError, b: &RdyError) {
    assert_eq!(
        (a.code, a.message.as_str(), a.fix.as_str()),
        (b.code, b.message.as_str(), b.fix.as_str())
    );
    assert_eq!(
        serde_json::to_string(&a.issues).unwrap(),
        serde_json::to_string(&b.issues).unwrap()
    );
}

fn all_pages(prepared: &PreparedPull, limit: usize) -> Vec<ObjectKey> {
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let page = prepared.page(after, limit).unwrap();
        out.extend(page.keys.iter().copied());
        after = page.next;
        if after.is_none() {
            return out;
        }
    }
}

fn get(
    prepared: &PreparedPull,
    keys: &[ObjectKey],
) -> (Vec<u8>, rdy_store::sync_stream::BatchReport) {
    let mut out = Vec::new();
    let report = prepared.write_keys(keys, &mut out).unwrap();
    (out, report)
}

#[test]
fn r59012_known_and_many_gets_walk_once() {
    let _clock = pin_clock(1_000_000);
    let w = world();
    let read = w.r.read();
    let (w0, p0) = stream_export::walks_for_tests();
    let prepared = PreparedPull::new(Arc::clone(&read), CANON, w.p2).unwrap();
    let (w1, p1) = stream_export::walks_for_tests();
    assert_eq!((w1 - w0, p1 - p0), (1, 1), "one prepare, one whole walk");

    // the known pages, whatever their size, make the old whole history's keys
    let whole = old_whole(&read, CANON, w.p2).unwrap().records;
    assert!(whole.len() > 12, "a history of some size");
    for limit in [1, 3, 7, whole.len(), MAX_KEYS] {
        assert_eq!(all_pages(&prepared, limit), whole, "pages of {limit}");
    }

    // disjoint gets (every 4 keys) and repeated ones (overlaps, repeats inside one get, and a
    // get twice): each batch is the old selection's bytes, and the keys the gets name make the
    // known keys
    let mut sets: Vec<Vec<ObjectKey>> = whole.chunks(4).map(<[_]>::to_vec).collect();
    let mut covered: BTreeSet<ObjectKey> = sets.iter().flatten().copied().collect();
    sets.push(whole[2..9].to_vec());
    sets.push([whole[3], whole[3], whole[5], whole[3]].to_vec());
    sets.push(sets[0].clone());
    sets.push(whole.iter().rev().copied().collect());
    for keys in &sets {
        let (bytes, report) = get(&prepared, keys);
        assert_eq!(bytes, old_keys(&read, CANON, w.p2, keys).unwrap());
        assert_eq!(check_batch(&bytes[..]).unwrap(), report);
        let named: BTreeSet<ObjectKey> = keys.iter().copied().collect();
        assert_eq!(report.keys.iter().copied().collect::<BTreeSet<_>>(), named);
        covered.extend(keys.iter().copied());
    }
    assert_eq!(covered, whole.iter().copied().collect::<BTreeSet<_>>());
    let (w2, p2) = stream_export::walks_for_tests();
    assert_eq!(
        (w2 - w1, p2 - p1),
        (0, 0),
        "the pages and every get read the one closure"
    );

    // the one-call forms hold nothing between calls: one walk each, where there were two
    let (a0, _) = stream_export::walks_for_tests();
    let mut out = Vec::new();
    stream_export::write_history_keys(&read, CANON, w.p2, &sets[0], &mut out).unwrap();
    assert_eq!(out, old_keys(&read, CANON, w.p2, &sets[0]).unwrap());
    let (a1, _) = stream_export::walks_for_tests();
    assert_eq!(a1 - a0, 1, "a one-call get walks once");
    let page = stream_export::history_page(&read, CANON, w.p2, None, None, 5).unwrap();
    assert_eq!(page.keys, whole[..5]);
    assert_eq!(page.next, Some(whole[4]));
    let (a2, _) = stream_export::walks_for_tests();
    assert_eq!(a2 - a1, 1, "a one-call page walks once");
}

#[test]
fn r59012_subset_order_proofs_and_deltas_are_unchanged() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    let mut reordered = 0;
    // before and after a removal: a removed payload travels as its placeholder, with its apply
    // and residue
    for removed in [false, true] {
        if removed {
            remove(&mut w.r, w.secret.clone());
        }
        let read = w.r.read();
        for (purpose, root) in [(CANON, w.p2), (CANON, w.p1), (VIEW, w.draft_head)] {
            let prepared = PreparedPull::new(Arc::clone(&read), purpose, root).unwrap();
            let whole = old_whole(&read, purpose, root).unwrap();
            assert_eq!(all_pages(&prepared, 5), whole.records);
            if purpose == CANON && root == w.p2 {
                let kinds_held: BTreeSet<u8> = whole.records.iter().map(|k| k.kind).collect();
                assert!(
                    kinds_held.contains(&kinds::SNAPSHOT_DELTA),
                    "a delta travels"
                );
                assert!(kinds_held.contains(&kinds::SIGNATURE));
                assert!(kinds_held.contains(&kinds::TAG));
                assert_eq!(kinds_held.contains(&kinds::REDACTION_APPLY), removed);
            }
            let n = whole.records.len();
            let mut sets: Vec<Vec<ObjectKey>> = vec![vec![], whole.records.clone()];
            // each key alone, then each pair of neighbours
            sets.extend(whole.records.iter().map(|k| vec![*k]));
            sets.extend(whole.records.windows(2).map(<[_]>::to_vec));
            // unrelated roots: every third, and every key but the root
            sets.push(whole.records.iter().copied().step_by(3).collect());
            sets.push(whole.records.iter().copied().skip(1).step_by(2).collect());
            sets.push(whole.records[..n - 1].to_vec());
            // a landing apart from its delta, and a delta apart from its landing
            for d in whole
                .records
                .iter()
                .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
            {
                sets.push(vec![*d]);
            }
            for l in whole.records.iter().filter(|k| k.kind == kinds::LANDING) {
                sets.push(vec![*l]);
            }
            for keys in &sets {
                let (bytes, report) = get(&prepared, keys);
                assert_eq!(
                    bytes,
                    old_keys(&read, purpose, root, keys).unwrap(),
                    "{keys:?}"
                );
                assert_eq!(check_batch(&bytes[..]).unwrap(), report);
                let named: BTreeSet<ObjectKey> = keys.iter().copied().collect();
                let filtered: Vec<ObjectKey> = whole
                    .records
                    .iter()
                    .copied()
                    .filter(|k| named.contains(k))
                    .collect();
                reordered += usize::from(report.keys != filtered);
                // every batch of one history carries one header
                assert_eq!(report.header.features, whole.features);
            }
        }
    }
    // the order of a subset is its own: filtering the whole order is not enough
    assert!(
        reordered > 0,
        "a subset whose order is not the whole order's"
    );
}

#[test]
fn r59012_bad_root_record_and_key_still_refuse() {
    let _clock = pin_clock(1_000_000);
    let w = world();
    let read = w.r.read();
    // a root the state does not hold, and a wrong purpose: prepare refuses as the whole walk did
    let unheld = Hash([9; 32]);
    let old = old_whole(&read, CANON, unheld).unwrap_err();
    let new = PreparedPull::new(Arc::clone(&read), CANON, unheld)
        .err()
        .unwrap();
    same_error(&old, &new);
    assert_eq!(new.code, "E_UNRESOLVED_REF");
    for purpose in [StreamPurpose::TagHistory, StreamPurpose::LinkDelivery] {
        let old = old_whole(&read, purpose, w.p2).unwrap_err();
        let new = PreparedPull::new(Arc::clone(&read), purpose, w.p2)
            .err()
            .unwrap();
        same_error(&old, &new);
        assert_eq!(new.code, "E_STREAM_PURPOSE");
    }
    // a view op is no canon root, and a landing is no view root
    assert!(PreparedPull::new(Arc::clone(&read), CANON, w.draft_head).is_err());
    assert!(PreparedPull::new(Arc::clone(&read), VIEW, w.p2).is_err());

    let prepared = PreparedPull::new(Arc::clone(&read), CANON, w.p2).unwrap();
    let whole = old_whole(&read, CANON, w.p2).unwrap().records;

    // a key outside the history: the old whole error, and no byte written
    let outside = ObjectKey {
        kind: kinds::BLOCK,
        hash: Hash([7; 32]),
    };
    let want = old_keys(&read, CANON, w.p2, &[whole[0], outside]).unwrap_err();
    let mut out = Vec::new();
    let got = prepared
        .write_keys(&[whole[0], outside], &mut out)
        .unwrap_err();
    same_error(&want, &got);
    assert_eq!(got.code, "E_UNRESOLVED_REF");
    assert!(out.is_empty());
    // the smallest missing key is the one named
    let outside2 = ObjectKey {
        kind: kinds::BLOCK,
        hash: Hash([3; 32]),
    };
    let got = prepared
        .write_keys(&[outside, outside2], &mut out)
        .unwrap_err();
    same_error(
        &old_keys(&read, CANON, w.p2, &[outside, outside2]).unwrap_err(),
        &got,
    );
    assert!(out.is_empty());

    // an absent cursor, as the page of a base-less read refuses it
    let cursor = prepared.page(Some(outside), 10).unwrap_err();
    let want =
        stream_export::history_page(&read, CANON, w.p2, None, Some(outside), 10).unwrap_err();
    same_error(&want, &cursor);
    assert_eq!(cursor.code, "E_UNRESOLVED_REF");

    // a limit out of range, a get over the key limit: E_LIMIT with the old text, no output
    for limit in [0, MAX_KEYS + 1] {
        let e = prepared.page(None, limit).unwrap_err();
        same_error(&limit_error(), &e);
    }
    let many: Vec<ObjectKey> = (0..=MAX_KEYS as u32)
        .map(|i| {
            let mut h = [0u8; 32];
            h[..4].copy_from_slice(&i.to_le_bytes());
            ObjectKey {
                kind: kinds::BLOCK,
                hash: Hash(h),
            }
        })
        .collect();
    let e = prepared.write_keys(&many, &mut out).unwrap_err();
    same_error(&limit_error(), &e);
    assert!(out.is_empty());
    // a repeated key counts once: the same key 100001 times is one key
    let repeated = vec![whole[0]; MAX_KEYS + 1];
    let (bytes, _) = get(&prepared, &repeated);
    assert_eq!(bytes, old_keys(&read, CANON, w.p2, &[whole[0]]).unwrap());

    // a stored payload that changed, and a stored dependency that is gone
    damaged_payload_refuses_at_prepare();
    missing_dependency_refuses_at_prepare();
}

/// The segment files of the project's store.
fn segment_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let store = dir
        .join(".rdy")
        .join(rdy_store::layout::STORE_DIR)
        .join("segments");
    let mut out: Vec<_> = std::fs::read_dir(&store)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

/// A stored payload that no longer holds the bytes of its hash, and a stored frame that its
/// dependents need but the store lacks, are refused at prepare with the whole walk's error.
fn damaged_payload_refuses_at_prepare() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$s", "parent": "$d", "body": format!("The launch code is {CANARY}.")}
        ]),
    );
    let p = land(&mut r, "work");
    let read = r.read();
    let healthy = PreparedPull::new(Arc::clone(&read), CANON, p).unwrap();
    assert!(!all_pages(&healthy, 100).is_empty());
    drop(healthy);
    // one byte of the stored prose changes
    let mut changed = 0;
    for f in segment_files(dir.path()) {
        let mut bytes = std::fs::read(&f).unwrap();
        if let Some(at) = bytes
            .windows(CANARY.len())
            .position(|w| w == CANARY.as_bytes())
        {
            bytes[at] ^= 1;
            std::fs::write(&f, &bytes).unwrap();
            changed += 1;
        }
    }
    assert!(changed > 0, "the prose is in a segment file");
    let old = old_whole(&read, CANON, p).unwrap_err();
    let new = PreparedPull::new(Arc::clone(&read), CANON, p)
        .err()
        .unwrap();
    same_error(&old, &new);
    assert_eq!(new.code, "E_IO");
    // the one-call forms refuse as they did
    let e = stream_export::history_page(&read, CANON, p, None, None, 10).unwrap_err();
    same_error(&old, &e);
}

#[test]
fn r59012_end_and_epoch_bound_the_operation() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    let read = w.r.read();
    let prepared = PreparedPull::new(Arc::clone(&read), CANON, w.p2).unwrap();
    let before = all_pages(&prepared, 4);
    let held = before.clone();
    let (bytes_before, _) = get(&prepared, &held);

    // attachments written after the capture: a tag, a signature of the root landing
    let t3 = tag(&mut w.r, "v3.0.0");
    let s2 = sign(&mut w.r, w.p2, 2);
    w.r.invalidate();
    assert_eq!(all_pages(&prepared, 4), before, "the captured end holds");
    let (bytes_after, _) = get(&prepared, &held);
    assert_eq!(bytes_after, bytes_before);
    // a key written later is outside this pull
    let late = ObjectKey {
        kind: kinds::SIGNATURE,
        hash: s2,
    };
    assert!(!before.contains(&late));
    let e = prepared.write_keys(&[late], &mut Vec::new()).unwrap_err();
    assert_eq!(e.code, "E_UNRESOLVED_REF");
    // a new pull sees the append
    let fresh = PreparedPull::new(w.r.read(), CANON, w.p2).unwrap();
    let now = all_pages(&fresh, 50);
    assert!(now.contains(&late));
    assert!(now.contains(&ObjectKey {
        kind: kinds::TAG,
        hash: t3
    }));
    assert!(now.len() > before.len());

    // dropping the prepared value releases what it captured
    let held_by_me = Arc::strong_count(&read);
    let again = PreparedPull::new(Arc::clone(&read), CANON, w.p2).unwrap();
    assert_eq!(Arc::strong_count(&read), held_by_me + 1);
    drop(again);
    assert_eq!(Arc::strong_count(&read), held_by_me);
    let weak = Arc::downgrade(&read);
    drop(prepared);
    drop(read);
    drop(fresh);
    assert!(
        weak.upgrade().is_none(),
        "nothing keeps the captured state once the pull is dropped"
    );

    // a removal after prepare refuses the pages and the gets, and writes nothing
    let read = w.r.read();
    let prepared = PreparedPull::new(Arc::clone(&read), CANON, w.p2).unwrap();
    let keys = all_pages(&prepared, 50);
    remove(&mut w.r, w.secret.clone());
    let e = prepared.page(None, 5).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
    let mut out = Vec::new();
    let e = prepared.write_keys(&keys, &mut out).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
    assert!(out.is_empty(), "nothing is written for a removed state");
    // a pull prepared after the removal reads it
    let after = PreparedPull::new(w.r.read(), CANON, w.p2).unwrap();
    let keys = all_pages(&after, 50);
    let (bytes, _) = get(&after, &keys);
    assert!(!bytes.windows(CANARY.len()).any(|b| b == CANARY.as_bytes()));
}

#[test]
fn r59012_incremental_bounds_do_not_expand() {
    let _clock = pin_clock(1_000_000);
    let w = world();
    let read = w.r.read();
    // an incremental page (a base the read holds) keeps the old walker: it walks once and
    // prepares no whole closure and key set
    let (w0, s0) = stream_export::walks_for_tests();
    let page = stream_export::history_page(&read, CANON, w.p2, Some(w.p1), None, 100).unwrap();
    let (w1, s1) = stream_export::walks_for_tests();
    assert_eq!((w1 - w0, s1 - s0), (1, 0));
    let old = history_closure_before(
        read.reader(),
        CANON,
        w.p2,
        &BTreeSet::new(),
        Some(w.p1),
        read.seq(),
    )
    .unwrap();
    assert_eq!(page.keys, old.records);
    let whole = old_whole(&read, CANON, w.p2).unwrap().records;
    assert!(
        page.keys.len() < whole.len(),
        "the base's history is left out"
    );
    // the view history walks the same way
    let p = stream_export::history_page(&read, VIEW, w.draft_head, None, None, 100).unwrap();
    let (_, s2) = stream_export::walks_for_tests();
    assert_eq!(
        s2 - s1,
        1,
        "a base-less page prepares once and keeps nothing"
    );
    assert!(!p.keys.is_empty());
    // a base the read lacks counts as none, as before
    let lacking =
        stream_export::history_page(&read, CANON, w.p2, Some(Hash([5; 32])), None, 1000).unwrap();
    assert_eq!(lacking.keys, whole);
    // an incremental export (a base) is not a prepared pull either
    let (_, s3) = stream_export::walks_for_tests();
    let mut out = Vec::new();
    let report =
        stream_export::write_history(&read, CANON, w.p2, Some(w.p1), &BTreeSet::new(), &mut out)
            .unwrap();
    assert_eq!(report.keys, old.records);
    assert_eq!(stream_export::walks_for_tests().1, s3);
}

/// The project's store, moved aside and loaded again with the block whose body holds `CANARY`
/// replaced by another of the same length (the block its change names is gone, a stored
/// dependency missing): every batch keeps its place and gets the digest of its frames.
fn lose_the_canary_block(dir: &std::path::Path) {
    use rdy_store::dump::Frame;
    let store = dir.join(".rdy").join(rdy_store::layout::STORE_DIR);
    let aside = dir.join("store-aside");
    std::fs::rename(&store, &aside).unwrap();
    let s = rdy_store::Store::open(&aside).unwrap();
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |mut b| {
            for f in b.frames.iter_mut() {
                if let Frame::Present {
                    kind,
                    hash,
                    payload,
                } = f
                    && *kind == kinds::BLOCK
                    && let Some(at) = payload
                        .windows(CANARY.len())
                        .position(|w| w == CANARY.as_bytes())
                {
                    // another block of the same length: the one the change names is gone
                    payload[at] ^= 1;
                    *hash = Hash::of(payload);
                }
            }
            b.digest = kinds::batch_digest(b.frames.iter().map(Frame::hash));
            batches.push(b);
            Ok(())
        })
        .unwrap();
    let features = s.features().clone();
    drop(s);
    let mut i = rdy_store::ingest::Ingest::create(&store).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.upgrade(&rdy_store::format::DumpStore::Features(features))
        .unwrap();
    i.finish().unwrap();
}

fn missing_dependency_refuses_at_prepare() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$s", "parent": "$d", "body": format!("The launch code is {CANARY}.")}
        ]),
    );
    let p = land(&mut r, "work");
    drop(r);
    lose_the_canary_block(dir.path());
    let r = Rdy::open(dir.path()).unwrap();
    let read = r.read();
    let old = old_whole(&read, CANON, p).unwrap_err();
    let new = PreparedPull::new(Arc::clone(&read), CANON, p)
        .err()
        .unwrap();
    same_error(&old, &new);
    assert_eq!(new.code, "E_UNRESOLVED_REF");
    let e = stream_export::write_history_keys(&read, CANON, p, &[], Vec::new()).unwrap_err();
    same_error(&old, &e);
}
