//! r59011 (D-D2NPN, D-DZDB7, D-KE83X, D-7K4Z0, D-8T7PA): a take or a move of batches that one
//! batch check of the same operation already checked. The check runs once per file; the take
//! and the move read each file's exact bytes again and keep every check over the union and the
//! holder, with the outcomes of an unchecked take. Index link counts found by the check stand
//! for the index's own decode, by each frame's key, and give the index a rebuild gives.
use crate::carried_fixture::copy_store;
use rdy_model::encoding::PLAN_DELTA_ENCODING;
use rdy_model::plan::{PlanAct, PlanAnswer, PlanHead};
use rdy_model::plan_encoding::{PlanDeltaV1, delta_body};
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, history_closure_before, write_batch};
use rdy_store::format::{
    FEATURE_SYNC_STATE, StoreFeatures, registered_features, required_features,
};
use rdy_store::hub_canon::{CanonMove, MoveFacts, Moved};
use rdy_store::kinds;
use rdy_store::object_refs::{ObjectKey, portable_refs_of};
use rdy_store::stream_error::StreamError;
use rdy_store::sync_stream::{
    CheckedBatch, StreamHeader, StreamPurpose, StreamReader, StreamRecord, StreamWriter,
    batch_checks_for_tests, check_batch_prepared_with,
};
use rdy_store::take_batch::{FetchTaken, FetchTarget, TakeError, index_link_decodes_for_tests};
use rdy_store::{LandRequest, Store, StoreReader};
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fs::File;
use std::path::PathBuf;

// test-hygiene: allow port the fetch target records this URL; nothing connects to it
const URL: &str = "https://127.0.0.1:9/r/demo";
const AT: i64 = 1_763_164_800_000;

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: if role == Role::Approver {
            "human:a".into()
        } else {
            "agent:w".into()
        },
        run: None,
        role,
        at_ms: 1,
    }
}

/// A change that creates block `n`: its CHANGE embeds the new block.
fn change(n: u32) -> Change {
    let block = |id: u32, body: String| Block {
        id: BlockId::parse(&format!("b-{id:05}")).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body,
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    };
    Change {
        view: "work".into(),
        ops: vec![Op::Create {
            block: block(n, format!("claim {n}")),
        }],
        deps: vec![],
        provenance: prov(Role::Writer),
        justification: None,
        message: format!("change {n}"),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// Lands one change that creates block `n` through the store's own landing path.
fn land(s: &mut Store, n: u32) -> Hash {
    s.commit_to_view("work", change(n)).unwrap();
    let p = s
        .propose("work", &format!("p{n}"), prov(Role::Writer))
        .unwrap();
    s.land_atomic(LandRequest {
        proposal: p,
        accept: None,
        approver: prov(Role::Approver),
        alt: None,
        relay: None,
        evidence: None,
        assessment: None,
    })
    .unwrap()
    .landing
}

/// A project with three landings, and a copy of it as it stood after the first (`q0`).
struct Src {
    dir: tempfile::TempDir,
    store: Store,
    q0: PathBuf,
    ls: [Hash; 3],
}

impl Src {
    fn new() -> Src {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("p");
        let mut s = Store::create(&p).unwrap();
        let l1 = land(&mut s, 1);
        drop(s);
        let q0 = dir.path().join("q0");
        copy_store(&p, &q0);
        let mut s = Store::open(&p).unwrap();
        let l2 = land(&mut s, 2);
        let l3 = land(&mut s, 3);
        Src {
            dir,
            store: s,
            q0,
            ls: [l1, l2, l3],
        }
    }

    /// A fresh copy of `q0`, opened.
    fn receiver(&self, name: &str) -> (PathBuf, Store) {
        let to = self.dir.path().join(name);
        copy_store(&self.q0, &to);
        let s = Store::open(&to).unwrap();
        (to, s)
    }

    /// Writes `bytes` to a new file `name` of the world and opens it.
    fn file(&self, name: &str, bytes: &[u8]) -> (PathBuf, File) {
        let p = self.dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        let f = File::open(&p).unwrap();
        (p, f)
    }

    fn fetch(&self) -> FetchTarget {
        FetchTarget {
            remote: "origin".into(),
            url: URL.into(),
            purpose: StreamPurpose::CanonHistory,
            root: self.ls[2],
            view: None,
        }
    }
}

/// The canon history of `root`, less `base`'s link closure, as one batch.
fn export(reader: &StoreReader, root: Hash, base: Option<Hash>) -> Vec<u8> {
    let found = history_closure_before(
        reader,
        StreamPurpose::CanonHistory,
        root,
        &BTreeSet::new(),
        base,
        END_OF_LOG,
    )
    .unwrap();
    let header = StreamHeader::canon_history(
        found.features,
        reader.repo_id().unwrap().unwrap(),
        root,
        found.prerequisites,
    );
    let mut source = StoreSource {
        reader,
        end: END_OF_LOG,
    };
    write_batch(&mut source, &header, &found.records, Vec::new())
        .unwrap()
        .0
}

/// `bytes` written again with its header changed by `head` and its records by `edit`.
fn rewrite(
    bytes: &[u8],
    head: impl FnOnce(&mut StreamHeader),
    edit: impl FnOnce(&mut Vec<StreamRecord>),
) -> Vec<u8> {
    let (mut r, mut header) = StreamReader::open_history(bytes).unwrap();
    let mut records = Vec::new();
    while let Some(rec) = r.next().unwrap() {
        records.push(rec);
    }
    head(&mut header);
    edit(&mut records);
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    for rec in &records {
        w.record(rec).unwrap();
    }
    w.finish().unwrap().0
}

/// The one batch check of `bytes`, kept.
fn capsule(bytes: &[u8]) -> CheckedBatch {
    check_batch_prepared_with(bytes, |_| Ok::<(), StreamError>(()), |_, _, _, _| Ok(())).unwrap()
}

/// The present records of a batch, as a move takes them.
fn present(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let (mut r, _) = StreamReader::open_history(bytes).unwrap();
    let mut out = Vec::new();
    while let Some(rec) = r.next().unwrap() {
        let raw = rec.raw_kind();
        if let StreamRecord::Present { payload, .. } = rec {
            out.push((raw, payload));
        }
    }
    out
}

/// The record of the delta of landing `l` in `records`, with its place.
fn delta_of(records: &[StreamRecord], l: Hash) -> (usize, SnapshotDelta) {
    records
        .iter()
        .enumerate()
        .find_map(|(i, r)| {
            let d: SnapshotDelta =
                (r.key().kind == kinds::SNAPSHOT_DELTA).then(|| r.typed().unwrap().unwrap())?;
            (d.landing == l).then_some((i, d))
        })
        .expect("the landing's delta")
}

/// A delta record of `d`, as a stream holds it.
fn delta_record(d: &SnapshotDelta) -> StreamRecord {
    let (raw, payload) = rdy_store::codec::item(kinds::SNAPSHOT_DELTA, d).unwrap();
    StreamRecord::Present {
        key: ObjectKey {
            kind: kinds::SNAPSHOT_DELTA,
            hash: Hash::of(&payload),
        },
        encoding: rdy_store::codec::split(raw, &payload).unwrap().version,
        payload,
    }
}

/// A fetch's outcome as two takes compare it: the taken counts, or the refusal's code and text.
type Got = Result<(u64, u64, Vec<String>), (String, String)>;

fn got(r: Result<FetchTaken, TakeError<Infallible>>) -> Got {
    r.map(|t| (t.new_objects, t.frames, t.tags_taken))
        .map_err(|e| match e {
            TakeError::Stream(e) => (e.code.to_string(), e.message),
            // the target is each receiver's own repo
            TakeError::RepoMismatch { source, .. } => ("repo".into(), source.to_string()),
            other => ("take".into(), other.to_string()),
        })
}

/// One fetch of `batches` into two fresh receivers: once unchecked, once with one batch check
/// of each file made first. Returns both outcomes, the checks of the unchecked take, and both
/// receivers. The checked take runs no check of its own.
fn both(
    src: &Src,
    name: &str,
    batches: &[Vec<u8>],
    target: &FetchTarget,
    receiver: &dyn Fn(&str) -> (PathBuf, Store),
) -> (Got, Got, u64, Store, Store) {
    let files = |tag: &str| -> Vec<File> {
        batches
            .iter()
            .enumerate()
            .map(|(i, b)| src.file(&format!("{name}-{tag}-{i}.rdysync"), b).1)
            .collect()
    };
    let (_, mut plain) = receiver(&format!("{name}-plain"));
    let (_, mut checked) = receiver(&format!("{name}-checked"));
    let (plain_end, checked_end) = (plain.log_end(), checked.log_end());
    let unchecked_files = files("plain");
    let before = batch_checks_for_tests();
    let a = got(plain.take_fetched::<Infallible>(&unchecked_files, target, |_, _| Ok(())));
    let plain_checks = batch_checks_for_tests() - before;
    let checked_files = files("checked");
    let before = batch_checks_for_tests();
    let caps: Vec<CheckedBatch> = batches.iter().map(|b| capsule(b)).collect();
    assert_eq!(
        batch_checks_for_tests() - before,
        batches.len() as u64,
        "{name}: one check for each file"
    );
    let refs: Vec<&CheckedBatch> = caps.iter().collect();
    let before = batch_checks_for_tests();
    let b =
        got(checked
            .take_fetched_checked::<Infallible>(&checked_files, &refs, target, |_, _| Ok(())));
    assert_eq!(
        batch_checks_for_tests() - before,
        0,
        "{name}: the checked take checks no file again"
    );
    if a.is_err() {
        assert_eq!(
            plain.log_end(),
            plain_end,
            "{name}: a refusal writes nothing"
        );
        assert_eq!(
            checked.log_end(),
            checked_end,
            "{name}: a refusal writes nothing"
        );
    }
    (a, b, plain_checks, plain, checked)
}

#[test]
fn r59011_fetch_receipts_keep_whole_upload_checks() {
    let src = Src::new();
    let [l1, l2, l3] = src.ls;
    let reader = src.store.reader();
    let b1 = export(&reader, l3, Some(l1));
    let b2 = export(&reader, l3, Some(l2));
    drop(reader);
    let fetch = src.fetch();
    let normal = |n: &str| src.receiver(n);

    // a whole fetch of two files: one check each, and the same records, frames and index
    let (a, b, checks, plain, checked) =
        both(&src, "ok", &[b1.clone(), b2.clone()], &fetch, &normal);
    assert!(a.is_ok(), "{a:?}");
    assert_eq!(a, b);
    assert_eq!(checks, 2, "the unchecked take checks each file itself");
    assert_eq!(
        plain.index_dump_for_tests().unwrap(),
        checked.index_dump_for_tests().unwrap()
    );
    assert_eq!(
        plain.reader().canon_head().unwrap(),
        checked.reader().canon_head().unwrap()
    );
    drop((plain, checked));

    // a key two files hold is one record
    let (a, b, _, _, _) = both(&src, "dup", &[b1.clone(), b1.clone()], &fetch, &normal);
    assert!(a.is_ok(), "{a:?}");
    assert_eq!(a, b);

    // a prerequisite neither the union nor the project holds
    let (a, b, _, _, _) = both(&src, "prereq", std::slice::from_ref(&b2), &fetch, &normal);
    assert_eq!(a.as_ref().unwrap_err().0, "E_UNRESOLVED_REF", "{a:?}");
    assert_eq!(a, b);

    // a root that is not the target's
    let other = FetchTarget {
        root: l2,
        ..fetch.clone()
    };
    let (a, b, _, _, _) = both(&src, "root", std::slice::from_ref(&b1), &other, &normal);
    assert_eq!(
        a.as_ref().unwrap_err().1,
        "fetch target does not match the batch root"
    );
    assert_eq!(a, b);

    // the files' headers name other features
    let more = rewrite(
        &b1,
        |h| {
            let f = registered_features()
                .into_iter()
                .filter(|f| f.name == FEATURE_SYNC_STATE)
                .collect::<Vec<_>>();
            h.features = h.features.merged(&f);
        },
        |_| {},
    );
    let (a, b, _, _, _) = both(&src, "features", &[b1.clone(), more], &fetch, &normal);
    assert!(
        a.as_ref().unwrap_err().1.ends_with("batch header differs"),
        "{a:?}"
    );
    assert_eq!(a, b);

    // a project of another repo
    let foreign = |n: &str| {
        let p = src.dir.path().join(n);
        let s = Store::create(&p).unwrap();
        (p, s)
    };
    let (a, b, _, _, _) = both(&src, "repo", std::slice::from_ref(&b1), &fetch, &foreign);
    assert_eq!(a.as_ref().unwrap_err().0, "repo", "{a:?}");
    assert_eq!(a, b);

    // a landing with no delta, one with two, and a delta that does not fold to its landing
    let no_delta = rewrite(
        &b1,
        |_| {},
        |rs| {
            let (i, _) = delta_of(rs, l3);
            rs.remove(i);
        },
    );
    let forged = |rs: &[StreamRecord]| {
        let (i, mut d) = delta_of(rs, l3);
        d.full = Hash([0x5A; 32]);
        (i, delta_record(&d))
    };
    let two = rewrite(
        &b1,
        |_| {},
        |rs| {
            let (_, r) = forged(rs);
            rs.push(r);
        },
    );
    let bad_chain = rewrite(
        &b1,
        |_| {},
        |rs| {
            let (i, r) = forged(rs);
            rs[i] = r;
        },
    );
    for (name, bytes) in [("nodelta", no_delta), ("twodeltas", two)] {
        let (a, b, _, _, _) = both(&src, name, &[bytes], &fetch, &normal);
        assert!(
            a.as_ref()
                .unwrap_err()
                .1
                .contains("expected exactly one snapshot delta"),
            "{name}: {a:?}"
        );
        assert_eq!(a, b, "{name}");
    }
    // a project's take keeps the delta rules it kept before (the fold of a delta to its
    // landing's state is the holder's and fsck's check): the same outcome either way
    let (a, b, _, _, _) = both(&src, "chain", &[bad_chain], &fetch, &normal);
    assert_eq!(a, b, "chain");
    // a checked batch of other bytes: the file is not the checked batch's
    let (_, mut q) = src.receiver("swap");
    let end = q.log_end();
    let cap = capsule(&b2);
    let (_, f) = src.file("swap.rdysync", &b1);
    let e = got(q.take_fetched_checked::<Infallible>(&[f], &[&cap], &fetch, |_, _| Ok(())))
        .unwrap_err();
    assert!(
        e.1.ends_with("input changed during take") || e.1.ends_with("batch header differs"),
        "{e:?}"
    );
    assert_eq!(q.log_end(), end);
    // and a count of checked batches other than the files'
    let (_, f) = src.file("count.rdysync", &b1);
    assert!(
        q.take_fetched_checked::<Infallible>(&[f], &[], &fetch, |_, _| Ok(()))
            .is_err()
    );
    assert_eq!(q.log_end(), end);
}

/// The present records of the canon history of `root` from the source, checked once.
fn move_batch(src: &Src, root: Hash, base: Option<Hash>) -> (Vec<u8>, CheckedBatch) {
    let bytes = export(&src.store.reader(), root, base);
    let cap = capsule(&bytes);
    (bytes, cap)
}

#[test]
fn r59011_move_and_seal_reuse_index_links() {
    let src = Src::new();
    let [l1, _, l3] = src.ls;
    let repo = src.store.reader().repo_id().unwrap().unwrap();
    let holder = |n: &str| Store::create_holder(&src.dir.path().join(n), repo).unwrap();
    let (mut hp, mut hc) = (holder("hp"), holder("hc"));
    // both holders take the first landing by an unchecked seed
    let (first, _) = move_batch(&src, l1, None);
    for h in [&mut hp, &mut hc] {
        let batch = h.reader().check_move_batch(present(&first), l1).unwrap();
        assert_eq!(
            h.move_canon(&batch, &CanonMove::Seed, None, AT).unwrap(),
            Moved::Moved { tip: l1 }
        );
    }
    // the whole history again, as an extend: the records of the first landing are duplicates
    // the write leaves out, so every checked count must stay with its own key
    let (full, cap) = move_batch(&src, l3, None);
    let decodes = index_link_decodes_for_tests();
    let batch = hp.reader().check_move_batch(present(&full), l3).unwrap();
    hp.move_canon(&batch, &CanonMove::Extend, Some(l1), AT)
        .unwrap();
    let plain_decodes = index_link_decodes_for_tests() - decodes;
    assert!(
        plain_decodes > 0,
        "an unchecked move decodes each object's links"
    );
    let decodes = index_link_decodes_for_tests();
    let batch = hc
        .reader()
        .check_move_batch_checked(present(&full), l3, MoveFacts::of(&[&cap]))
        .unwrap();
    assert_eq!(
        hc.move_canon(&batch, &CanonMove::Extend, Some(l1), AT)
            .unwrap(),
        Moved::Moved { tip: l3 }
    );
    assert_eq!(
        index_link_decodes_for_tests() - decodes,
        0,
        "a checked move indexes the links its check found"
    );
    // the same index, the same holder counts, before and after a rebuild, and the holder check
    let dump = hc.index_dump_for_tests().unwrap();
    assert_eq!(dump, hp.index_dump_for_tests().unwrap());
    let counts = hc.holder_counts().unwrap();
    assert_eq!(counts, hp.holder_counts().unwrap());
    hc.reindex().unwrap();
    assert_eq!(hc.index_dump_for_tests().unwrap(), dump);
    assert_eq!(hc.holder_counts().unwrap(), counts);
    hc.reader().check_holder().unwrap();
    hp.reader().check_holder().unwrap();

    // a landing under snapshot_delta keeps its state among its index links, and an embedded
    // CHANGE its block links: each checked count is the count of the frame's own links with no
    // feature, as the index's decode counts them
    let header = &cap.report().header;
    let none = StoreFeatures::of(vec![]);
    let records = present(&full);
    let mut kinds_seen = BTreeSet::new();
    for (i, f) in cap.frames().iter().enumerate() {
        let (raw, payload) = &records[i];
        assert_eq!((f.raw_kind(), f.key().hash), (*raw, Hash::of(payload)));
        let own = portable_refs_of(&none, *raw, payload).unwrap().len() as u64;
        let want = if f.key().kind == kinds::BLOB_CHUNK {
            0
        } else {
            own
        };
        assert_eq!(f.index_links(), want, "{}", f.key());
        kinds_seen.insert(f.key().kind);
        if f.key().kind == kinds::LANDING {
            let under = portable_refs_of(&header.features, *raw, payload)
                .unwrap()
                .len() as u64;
            assert_eq!(f.index_links(), under + 1, "the landing counts its state");
        }
    }
    for k in [
        kinds::CHANGE,
        kinds::LANDING,
        kinds::SNAPSHOT_DELTA,
        kinds::PROPOSAL,
    ] {
        assert!(kinds_seen.contains(&k), "the history holds kind {k}");
    }

    // a delta PLAN and a dropped record: the checked count is the frame's own, and 0 for a drop
    let answer = PlanAnswer {
        item: Hash([3; 32]),
        act: PlanAct::Done,
        reason: None,
        subject: None,
        cause: Hash([0x52; 32]),
        by: prov(Role::Writer),
    };
    let delta = PlanDeltaV1 {
        stable: Hash([1; 32]),
        prev: Hash([2; 32]),
        view: "work".into(),
        proposal: None,
        answers_set: vec![answer],
        answers_remove: vec![],
        heads: vec![PlanHead {
            repo: "app".into(),
            head: Some("a".repeat(40)),
        }],
        provenance: prov(Role::Writer),
        seal: None,
    };
    let (raw, payload) =
        rdy_store::codec::frame(kinds::PLAN, PLAN_DELTA_ENCODING, delta_body(&delta)).unwrap();
    let plan_key = ObjectKey {
        kind: kinds::PLAN,
        hash: Hash::of(&payload),
    };
    let features = StoreFeatures::of(required_features(raw, &payload));
    let mut prerequisites: BTreeSet<ObjectKey> = portable_refs_of(&features, raw, &payload)
        .unwrap()
        .into_iter()
        .map(|r| ObjectKey {
            kind: r.kind.unwrap(),
            hash: r.hash,
        })
        .collect();
    let apply = ObjectKey {
        kind: kinds::REDACTION_APPLY,
        hash: Hash([8; 32]),
    };
    let root = Hash([7; 32]);
    prerequisites.extend([
        apply,
        ObjectKey {
            kind: kinds::LANDING,
            hash: root,
        },
    ]);
    let header = StreamHeader::canon_history(
        features.clone(),
        repo,
        root,
        prerequisites.into_iter().collect(),
    );
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    w.record(&StreamRecord::Present {
        key: plan_key,
        encoding: PLAN_DELTA_ENCODING,
        payload: payload.clone(),
    })
    .unwrap();
    w.record(&StreamRecord::Dropped {
        key: ObjectKey {
            kind: kinds::BLOB_CHUNK,
            hash: Hash([9; 32]),
        },
        encoding: 0,
        original_len: 10,
        apply: apply.hash,
        residue: None,
    })
    .unwrap();
    let synthetic = w.finish().unwrap().0;
    let cap = capsule(&synthetic);
    let frames = cap.frames();
    assert_eq!(
        frames[0].index_links(),
        portable_refs_of(&none, raw, &payload).unwrap().len() as u64
    );
    assert!(frames[0].index_links() > 0);
    assert_eq!(
        cap.refs(0),
        portable_refs_of(&features, raw, &payload).unwrap()
    );
    assert!(frames[1].is_dropped());
    assert_eq!(frames[1].index_links(), 0);
    assert_eq!(frames[1].proof(), Some(apply));
}

#[test]
fn r59011_seal_reread_and_durability_still_refuse() {
    let src = Src::new();
    let [l1, _, l3] = src.ls;
    let b1 = export(&src.store.reader(), l3, Some(l1));
    let fetch = src.fetch();
    let cap = capsule(&b1);
    let message = |e: TakeError<Infallible>| match e {
        TakeError::Stream(e) => format!("{}: {}", e.code, e.message),
        TakeError::Store(e) => format!("store: {e}"),
        other => other.to_string(),
    };

    // a file changed at the append: the seal's re-read refuses it and nothing stays
    for checked in [false, true] {
        let (dir, mut q) = src.receiver(&format!("changed-{checked}"));
        let end = q.log_end();
        let (path, f) = src.file(&format!("changed-{checked}.rdysync"), &b1);
        let mut bytes = b1.clone();
        for b in bytes.iter_mut().skip(12) {
            *b = 0;
        }
        q.on_take_append_for_tests(move |appended| {
            if appended == 0 {
                std::fs::write(&path, &bytes).unwrap();
            }
        });
        let e = if checked {
            q.take_fetched_checked::<Infallible>(&[f], &[&cap], &fetch, |_, _| Ok(()))
        } else {
            q.take_fetched::<Infallible>(&[f], &fetch, |_, _| Ok(()))
        }
        .map(|_| ())
        .map_err(message)
        .unwrap_err();
        assert!(
            e.starts_with("E_BAD_RECORD") && e.ends_with("input changed during take"),
            "{e}"
        );
        assert_eq!(q.log_end(), end);
        assert_eq!(q.reader().sync_canon("origin").unwrap(), None);
        drop(q);
        let q = Store::open(&dir).unwrap();
        assert_eq!(q.log_end(), end);
    }

    // an append, a sync and an index failure: the same refusal checked or not, nothing
    // acknowledged, the store reopens as it was, and the fetch then passes
    type Fault = fn(&mut Store);
    let faults: [(&str, Fault); 3] = [
        ("append", |s| s.fail_next_append_after(10)),
        ("sync", |s| s.fail_next_sync()),
        ("index", |s| s.fail_next_index_apply()),
    ];
    for (name, fault) in faults {
        let mut outcomes = Vec::new();
        for checked in [false, true] {
            let (dir, mut q) = src.receiver(&format!("{name}-{checked}"));
            let end = q.log_end();
            let (_, f) = src.file(&format!("{name}-{checked}.rdysync"), &b1);
            fault(&mut q);
            let e = if checked {
                q.take_fetched_checked::<Infallible>(&[f], &[&cap], &fetch, |_, _| Ok(()))
            } else {
                q.take_fetched::<Infallible>(&[f], &fetch, |_, _| Ok(()))
            }
            .map(|_| ())
            .map_err(message)
            .unwrap_err();
            assert_eq!(q.log_end(), end, "{name}: nothing acknowledged");
            assert_eq!(q.reader().sync_canon("origin").unwrap(), None, "{name}");
            drop(q);
            let mut q = Store::open(&dir).unwrap();
            assert_eq!(q.log_end(), end, "{name}: the reopened store is as it was");
            let (_, f) = src.file(&format!("{name}-{checked}-again.rdysync"), &b1);
            let again = capsule(&b1);
            let t = q
                .take_fetched_checked::<Infallible>(&[f], &[&again], &fetch, |_, _| Ok(()))
                .unwrap();
            assert!(t.frames > 0, "{name}: the fetch passes after the failure");
            outcomes.push(e);
        }
        assert_eq!(outcomes[0], outcomes[1], "{name}: the same refusal");
    }
}
