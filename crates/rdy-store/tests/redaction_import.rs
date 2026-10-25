//! A holder's removals by their logical tuples (plan HA1 Task 11; D-3M81W: rdy "drops their
//! bytes and keeps their hashes"; D-CC90E: a removal needs its approval; O2): a take erases
//! every receiver copy of an approved tuple, found by the holder's own log, never by a source
//! place; an uncovered embedding refuses until a sibling removal covers it; all-versions adds
//! nothing at the receiver; the open, a reindex and the raw-write guard derive and reuse the same
//! authorization with no stored map; a removal sealed before its erasure finishes recovers at the
//! next open; the receiver's scan is bounded.
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::redaction::StoredFrame;
use rdy_store::redaction_import::{
    MAX_RECEIVER_BYTES, MAX_RECEIVER_FRAMES, MAX_RECEIVER_SPANS, ReceiverCaps,
    set_receiver_caps_for_tests,
};
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
use rdy_store::segment::{Location, SegmentLog};
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::StreamHeader;
use rdy_store::take_batch::{BatchTaken, TagChoice, TakeError, UploadFacts, UploadTarget};
use rdy_store::{Store, StoreError};
use redaction_fixture::{
    Fixture, FixtureOptions, ORIGIN, Proof, RequestSpec, authorized_batch_fixture, enc,
    residue_frame, segment_bytes,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

/// The removed block's body; no holder segment may keep it after the removal's take.
const CANARY: &str = "canary alpha secret";

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role,
        at_ms: 1,
    }
}

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: prov("agent:w", Role::Writer),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn creates(blocks: &[&Block]) -> Vec<Op> {
    blocks
        .iter()
        .map(|b| Op::Create {
            block: (*b).clone(),
        })
        .collect()
}

/// Commits `ops` to `view`, proposes and lands it; returns the change and the landing.
fn land(path: &Path, view: &str, ops: Vec<Op>) -> (Hash, Hash) {
    let mut s = Store::open(path).unwrap();
    let c = s.commit_to_view(view, change(view, ops)).unwrap();
    let p = s
        .propose(view, "work", prov("agent:w", Role::Writer))
        .unwrap();
    let l = s.land(&p, prov("human:alice", Role::Approver)).unwrap();
    (c, l)
}

fn tag(path: &Path, name: &str, landing: Hash) -> Hash {
    let mut s = Store::open(path).unwrap();
    s.put_tag(&Tag {
        name: name.into(),
        landing,
        message: None,
        tagged_by: prov("human:alice", Role::Approver),
    })
    .unwrap()
}

/// An approved removal of `targets` in view `view` of the closed project at `path`, finished by
/// an open.
fn remove(path: &Path, view: &str, targets: Vec<RedactTarget>) -> Fixture {
    let f = authorized_batch_fixture(
        path,
        &[RequestSpec::new(view, targets).at(60)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    drop(Store::open(path).unwrap());
    f
}

/// One batch file of `tag`'s whole closure in the project at `path`.
fn upload(path: &Path, tag: Hash, out: &Path) -> Vec<File> {
    let s = Store::open(path).unwrap();
    let r = s.reader();
    let c = closure(&r, tag, &BTreeSet::new()).unwrap();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        r.repo_id().unwrap().unwrap(),
        tag,
        c.prerequisites,
    );
    let mut source = StoreSource {
        reader: &r,
        end: END_OF_LOG,
    };
    write_batch(&mut source, &header, &c.records, File::create(out).unwrap()).unwrap();
    vec![File::open(out).unwrap()]
}

fn incoming(_: &mut dyn ObjectSource, _: &UploadFacts) -> Result<TagChoice, StreamError> {
    Ok(TagChoice::Incoming)
}

fn take(
    h: &mut Store,
    files: &[File],
    name: &str,
    landing: Hash,
) -> Result<BatchTaken, TakeError<StreamError>> {
    h.take_batches(
        files,
        &UploadTarget {
            tag: name.into(),
            landing,
        },
        incoming,
    )
}

/// Every frame of the holder's log: location, kind, hash, whether its payload is dropped.
fn frames(h: &Store) -> Vec<(Location, u8, Hash, bool)> {
    let mut out = vec![];
    h.reader()
        .for_each_record_from(ORIGIN, h.log_end(), |loc, f, _| {
            // by the logical kind (`rdy_store::codec`)
            out.push(match f {
                StoredFrame::Present { kind, hash, .. } => {
                    (loc, rdy_store::codec::logical_kind(kind), hash, false)
                }
                StoredFrame::Dropped { kind, hash, .. } => {
                    (loc, rdy_store::codec::logical_kind(kind), hash, true)
                }
            });
            Ok(())
        })
        .unwrap();
    out
}

/// The locations of the frames of `k`, and whether each is dropped.
fn copies(h: &Store, k: ObjectKey) -> Vec<(Location, bool)> {
    frames(h)
        .into_iter()
        .filter(|(_, kind, hash, _)| key(*kind, *hash) == k)
        .map(|(l, _, _, d)| (l, d))
        .collect()
}

fn holds_canary(dir: &Path) -> bool {
    segment_bytes(dir)
        .values()
        .any(|b| b.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()))
}

/// Appends `items` as one sealed batch to the closed store at `dir`, as a raw log write.
fn append_raw(dir: &Path, items: &[(u8, Vec<u8>)]) -> Vec<Location> {
    let (mut log, q) =
        SegmentLog::open_marked(&dir.join("segments"), 64 * 1024 * 1024, None).unwrap();
    assert!(q.is_empty());
    let placed = log.append_batch(items).unwrap();
    log.sync().unwrap();
    placed.into_iter().map(|(l, _)| l).collect()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

/// The base history: a project with a padding block, then one change of the secret block and
/// another, landed and tagged `v1`; a holder that took `v1` (after `prepare` ran on it, closed);
/// then the project's approved removal of the secret version, landed and tagged `v2`. The
/// holder holds the secret intact until it takes `v2`.
struct Base {
    dir: tempfile::TempDir,
    src: PathBuf,
    holder: PathBuf,
    secret: Block,
    other: Block,
    change: Hash,
    v1: (Hash, Hash),
    v2: (Hash, Hash),
    fixture: Fixture,
}

fn base(pad: usize) -> Base {
    base_with(pad, |_| {})
}

fn base_with(pad: usize, prepare: impl FnOnce(&Path)) -> Base {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let mut s = Store::create(&src).unwrap();
    if pad > 0 {
        s.put(kinds::BLOCK, &block("b-00009", &"p".repeat(pad)))
            .unwrap();
    }
    drop(s);
    let secret = block("b-00001", CANARY);
    let other = block("b-00002", "beta");
    let (change, l1) = land(&src, "work", creates(&[&secret, &other]));
    let t1 = tag(&src, "v1", l1);
    let holder = dir.path().join("holder");
    let repo = Store::open(&src).unwrap().repo_id().unwrap().unwrap();
    drop(Store::create_holder(&holder, repo).unwrap());
    prepare(&holder);
    {
        let mut h = Store::open(&holder).unwrap();
        let files = upload(&src, t1, &dir.path().join("v1-first.rdysync"));
        let got = take(&mut h, &files, "v1", l1).unwrap();
        assert_eq!(got.redacted_objects, 0);
    }
    let fixture = remove(
        &src,
        "work",
        vec![RedactTarget::Version {
            id: secret.id.clone(),
            version: secret.version_hash(),
        }],
    );
    let l2 = fixture.landing.unwrap();
    let t2 = tag(&src, "v2", l2);
    Base {
        dir,
        src,
        holder,
        secret,
        other,
        change,
        v1: (t1, l1),
        v2: (t2, l2),
        fixture,
    }
}

impl Base {
    fn file(&self, tag: Hash, name: &str) -> Vec<File> {
        upload(
            &self.src,
            tag,
            &self.dir.path().join(format!("{name}.rdysync")),
        )
    }

    fn take_v1(&self, h: &mut Store) -> BatchTaken {
        take(h, &self.file(self.v1.0, "v1"), "v1", self.v1.1).unwrap()
    }

    fn take_v2(&self, h: &mut Store) -> Result<BatchTaken, TakeError<StreamError>> {
        take(h, &self.file(self.v2.0, "v2"), "v2", self.v2.1)
    }

    fn secret_key(&self) -> ObjectKey {
        key(kinds::BLOCK, self.secret.version_hash())
    }

    fn change_key(&self) -> ObjectKey {
        key(kinds::CHANGE, self.change)
    }

    fn request(&self) -> Hash {
        self.fixture.requests[0].request
    }
}

#[test]
fn ha11_all_local_copies_of_approved_tuples_are_removed() {
    let b = base(0);
    let h = Store::open(&b.holder).unwrap();
    let secret_bytes = h.get_raw(&b.secret.version_hash()).unwrap().1;
    let (change_kind, change_bytes) = h.get_kind_raw(kinds::CHANGE, &b.change).unwrap();
    drop(h);
    // a second physical copy of each removed object, in a later batch of the holder's log
    append_raw(
        &b.holder,
        &[(kinds::BLOCK, secret_bytes), (change_kind, change_bytes)],
    );
    let mut h = Store::open(&b.holder).unwrap();
    assert_eq!(copies(&h, b.secret_key()).len(), 2);
    assert_eq!(copies(&h, b.change_key()).len(), 2);
    assert!(holds_canary(&b.holder));
    let got = b.take_v2(&mut h).unwrap();
    assert_eq!(got.redacted_objects, 2);
    // every copy is dropped, and no byte of the secret is left
    for k in [b.secret_key(), b.change_key()] {
        let c = copies(&h, k);
        assert_eq!(c.len(), 2, "{k}");
        assert!(c.iter().all(|(_, dropped)| *dropped), "{k}: {c:?}");
    }
    assert!(!holds_canary(&b.holder));
    assert!(matches!(
        h.get_raw(&b.secret.version_hash()),
        Err(StoreError::Redacted { request }) if request == b.request()
    ));
    // the other block of the same change stays
    assert_eq!(h.get_block(&b.other.version_hash()).unwrap().body, "beta");
    // the projection reads the residue
    let stub = h.get_block(&b.secret.version_hash()).unwrap();
    assert_ne!(stub.body, CANARY);
    // and the holder reopens with them
    drop(h);
    let h = Store::open(&b.holder).unwrap();
    assert!(copies(&h, b.secret_key()).iter().all(|(_, d)| *d));
}

#[test]
fn ha11_source_offsets_are_never_receiver_offsets() {
    // the source puts its secret frame far into its log, and the holder puts an unrelated frame
    // exactly where the source's secret frame is, before it takes v1
    let unrelated = block("b-00007", "unrelated");
    let placed = std::cell::Cell::new(None);
    let b = base_with(8192, |holder| {
        let place = secret_place_of_padded_source();
        let h = Store::open(holder).unwrap();
        let end = h.log_end();
        drop(h);
        assert_eq!(end.segment, place.segment);
        let pad = (place.offset - end.offset - 37) as usize;
        let at = append_raw(
            holder,
            &[
                (kinds::BLOB_CHUNK, vec![7u8; pad]),
                (kinds::BLOCK, enc(&unrelated)),
            ],
        );
        placed.set(Some(at[1]));
    });
    let spans = &b.fixture.requests[0].draft.request.manifest.spans;
    let place = spans
        .iter()
        .find(|s| s.kind == kinds::BLOCK)
        .map(|s| Location {
            segment: s.place.segment,
            offset: s.place.offset,
        })
        .unwrap();
    assert_eq!(placed.get(), Some(place));
    let mut h = Store::open(&b.holder).unwrap();
    let mine = copies(&h, b.secret_key());
    assert_eq!(mine.len(), 1);
    assert_ne!(mine[0].0, place);
    b.take_v2(&mut h).unwrap();
    // the frame at the source's place is intact; the holder's own copy is dropped
    let at_place: Vec<_> = frames(&h).into_iter().filter(|f| f.0 == place).collect();
    assert_eq!(
        at_place,
        vec![(place, kinds::BLOCK, unrelated.version_hash(), false)]
    );
    assert_eq!(
        h.get_block(&unrelated.version_hash()).unwrap().body,
        "unrelated"
    );
    assert_eq!(copies(&h, b.secret_key()), vec![(mine[0].0, true)]);
    // every dropped frame of the holder is a copy of an approved key
    let approved = BTreeSet::from([b.secret_key(), b.change_key()]);
    for (_, kind, hash, dropped) in frames(&h) {
        assert_eq!(
            dropped,
            approved.contains(&key(kind, hash)),
            "{kind}:{hash}"
        );
    }
}

/// hc1: a holder's dump names the lowest local span of its applies, which HA1's logical rule
/// resolves from the holder's own log (every local copy of an approved key), never a place of
/// the source's manifest. The writer's facts (from the store's authorization) and the reader's
/// (from the file) agree, and the dump reads back with them.
#[test]
fn hc1_a_holder_dump_names_its_lowest_local_span() {
    use rdy_store::dump::{DumpHeader, DumpReader, DumpWriter, RemovalFacts, RemovalTally};
    use rdy_store::dump_chain::DumpChainHeader;
    let b = base(8192);
    let mut h = Store::open(&b.holder).unwrap();
    b.take_v2(&mut h).unwrap();
    let end = h.log_end();
    let r = h.reader();
    let mut tally = RemovalTally::default();
    let mut batches = vec![];
    r.for_each_batch(end, |bt| {
        tally.observe(&bt)?;
        batches.push(bt);
        Ok(())
    })
    .unwrap();
    let local = [b.secret_key(), b.change_key()]
        .iter()
        .flat_map(|k| copies(&h, *k))
        .map(|(l, dropped)| {
            assert!(dropped);
            l
        })
        .min();
    let source = b.fixture.requests[0]
        .draft
        .request
        .manifest
        .spans
        .iter()
        .map(|s| Location {
            segment: s.place.segment,
            offset: s.place.offset,
        })
        .min();
    assert!(local.is_some());
    assert_ne!(local, source, "the source's places are not the holder's");
    let want = RemovalFacts {
        removals: 1,
        first_removed: local,
    };
    assert_eq!(tally.resolved(&r, end).unwrap(), want, "the writer's facts");
    assert_eq!(tally.facts().unwrap(), want, "the file's facts");
    let header = DumpHeader {
        store: rdy_store::format::DumpStore::Features(rdy_store::format::baseline_features()),
        log_end: end,
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(
            r.repo_id().unwrap().unwrap(),
            r.batch_marker_at(end).unwrap().unwrap(),
        )),
    };
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    for bt in &batches {
        w.batch(bt).unwrap();
    }
    let (bytes, _) = w.finish(want).unwrap();
    let (mut rd, _, _) = DumpReader::open(&bytes[..]).unwrap();
    while rd.next().unwrap().is_some() {}
    assert_eq!(rd.trailer().unwrap().first_removed, local);
}

/// Where a base source padded with 8192 bytes holds its secret block's frame: the same
/// history built again in a scratch project.
fn secret_place_of_padded_source() -> Location {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let mut s = Store::create(&src).unwrap();
    s.put(kinds::BLOCK, &block("b-00009", &"p".repeat(8192)))
        .unwrap();
    drop(s);
    let secret = block("b-00001", CANARY);
    land(&src, "work", creates(&[&secret, &block("b-00002", "beta")]));
    let s = Store::open(&src).unwrap();
    copies(&s, key(kinds::BLOCK, secret.version_hash()))[0].0
}

/// Two projects of one repo that diverged from one empty store: A and B.
fn twins() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, RepoId) {
    let dir = tempfile::tempdir().unwrap();
    let s0 = dir.path().join("s0");
    let repo = {
        let s = Store::create(&s0).unwrap();
        s.repo_id().unwrap().unwrap()
    };
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    copy_dir(&s0, &a);
    copy_dir(&s0, &b);
    let holder = dir.path().join("holder");
    drop(Store::create_holder(&holder, repo).unwrap());
    (dir, a, b, holder, repo)
}

#[test]
fn ha11_uncovered_embedding_refuses_but_sibling_removal_unblocks() {
    let (dir, a, b, holder, _) = twins();
    let secret = block("b-00001", CANARY);
    let version = secret.version_hash();
    // the same block version, embedded by a different change in each project
    let (change_a, la1) = land(&a, "work", creates(&[&secret]));
    let (change_b, lb1) = land(
        &b,
        "other",
        creates(&[&secret, &block("b-00003", "b side")]),
    );
    assert_ne!(change_a, change_b);
    let tb1 = tag(&b, "b1", lb1);
    let mut h = Store::open(&holder).unwrap();
    take(&mut h, &upload(&b, tb1, &dir.path().join("b1")), "b1", lb1).unwrap();
    let _ = la1;
    // A removes the version: its approval covers its own change, not B's
    let fa = remove(
        &a,
        "work",
        vec![RedactTarget::Version {
            id: secret.id.clone(),
            version,
        }],
    );
    let la2 = fa.landing.unwrap();
    let ta2 = tag(&a, "a2", la2);
    let a2 = dir.path().join("a2");
    let before = segment_bytes(&holder);
    match take(&mut h, &upload(&a, ta2, &a2), "a2", la2) {
        Err(TakeError::UncoveredChange(c)) => assert_eq!(c, change_b),
        other => panic!("{other:?}"),
    }
    assert_eq!(segment_bytes(&holder), before);
    assert!(holds_canary(&holder));
    // B's own approved removal of the version covers B's change
    let fb = remove(
        &b,
        "other",
        vec![RedactTarget::Version {
            id: secret.id.clone(),
            version,
        }],
    );
    let lb2 = fb.landing.unwrap();
    let tb2 = tag(&b, "b2", lb2);
    let got = take(&mut h, &upload(&b, tb2, &dir.path().join("b2")), "b2", lb2).unwrap();
    assert_eq!(got.redacted_objects, 2);
    assert!(
        copies(&h, key(kinds::CHANGE, change_b))
            .iter()
            .all(|(_, d)| *d)
    );
    assert!(!holds_canary(&holder));
    // now A's upload passes: the sibling removal already dropped B's change
    let got = take(&mut h, &upload(&a, ta2, &a2), "a2", la2).unwrap();
    assert!(got.new_objects > 0);
    assert_eq!(copies(&h, key(kinds::CHANGE, change_a)).len(), 1);
    assert!(copies(&h, key(kinds::CHANGE, change_a))[0].1);
    assert!(!holds_canary(&holder));
    drop(h);
    let h = Store::open(&holder).unwrap();
    assert!(
        copies(&h, key(kinds::BLOCK, version))
            .iter()
            .all(|(_, d)| *d)
    );
}

#[test]
fn ha11_allversions_does_not_expand_at_receiver() {
    let (dir, a, b, holder, _) = twins();
    // two versions of one block id: A's and B's
    let va = block("b-00001", CANARY);
    let vb = block("b-00001", "b keeps this version");
    let (_, la1) = land(&a, "work", creates(&[&va]));
    let (change_b, lb1) = land(&b, "work", creates(&[&vb]));
    let _ = la1;
    let tb1 = tag(&b, "b1", lb1);
    let mut h = Store::open(&holder).unwrap();
    take(&mut h, &upload(&b, tb1, &dir.path().join("b1")), "b1", lb1).unwrap();
    // A removes every version of the id it knows
    let fa = remove(&a, "work", vec![RedactTarget::AllVersions(va.id.clone())]);
    let la2 = fa.landing.unwrap();
    let ta2 = tag(&a, "a2", la2);
    let got = take(&mut h, &upload(&a, ta2, &dir.path().join("a2")), "a2", la2).unwrap();
    assert!(got.redacted_objects > 0);
    // B's version and the change that wrote it are untouched
    assert_eq!(
        h.get_block(&vb.version_hash()).unwrap().body,
        "b keeps this version"
    );
    assert!(
        copies(&h, key(kinds::BLOCK, vb.version_hash()))
            .iter()
            .all(|(_, d)| !*d)
    );
    assert!(
        copies(&h, key(kinds::CHANGE, change_b))
            .iter()
            .all(|(_, d)| !*d)
    );
    assert!(
        h.redaction_of(kinds::BLOCK, &vb.version_hash())
            .unwrap()
            .is_none()
    );
    // A's version is removed
    assert!(
        copies(&h, key(kinds::BLOCK, va.version_hash()))
            .iter()
            .all(|(_, d)| *d)
    );
    assert!(!holds_canary(&holder));
}

#[test]
fn ha11_reindex_rebuilds_logical_authorization_without_maps() {
    let b = base(0);
    let mut h = Store::open(&b.holder).unwrap();
    b.take_v2(&mut h).unwrap();
    let dump = h.index_dump_for_tests().unwrap();
    let reads = |h: &Store| {
        (
            h.get_raw(&b.secret.version_hash())
                .map_err(|e| e.to_string()),
            h.get_block(&b.secret.version_hash()).unwrap(),
            h.change(&b.change).unwrap(),
            h.redaction_of(kinds::BLOCK, &b.secret.version_hash())
                .unwrap(),
            copies(h, b.secret_key()),
        )
    };
    let want = reads(&h);
    assert!(want.0.is_err());
    // no record of a kind the store does not assign: the authorization is derived
    assert!(frames(&h).iter().all(|(_, kind, _, _)| *kind != 12));
    drop(h);
    // the derived index is deleted; the open rebuilds it from the log
    std::fs::remove_file(b.holder.join("index.redb")).unwrap();
    let mut h = Store::open(&b.holder).unwrap();
    assert_eq!(reads(&h), want);
    assert_eq!(h.index_dump_for_tests().unwrap(), dump);
    h.reindex().unwrap();
    assert_eq!(reads(&h), want);
    assert_eq!(h.index_dump_for_tests().unwrap(), dump);
    h.check_redaction_index_for_tests().unwrap();
    h.check_meta().unwrap();
    // the holder's batches load into a new store, and its drops are authorized by the same
    // derivation (the ingest of a load)
    let loaded = b.dir.path().join("loaded");
    let mut batches = vec![];
    h.reader()
        .for_each_batch(h.log_end(), |batch| {
            batches.push(batch);
            Ok(())
        })
        .unwrap();
    drop(h);
    let mut ingest = rdy_store::ingest::Ingest::create(&loaded).unwrap();
    for (n, batch) in batches.iter().enumerate() {
        ingest.batch(n as u64 + 1, batch).unwrap();
    }
    ingest
        .upgrade(&rdy_store::format::DumpStore::Features(
            rdy_store::format::baseline_features(),
        ))
        .unwrap();
    ingest.finish().unwrap();
    let l = Store::open(&loaded).unwrap();
    assert_eq!(reads(&l).0, want.0);
    assert_eq!(segment_bytes(&loaded), segment_bytes(&b.holder));
}

#[test]
fn ha11_later_raw_write_cannot_restore_removed_bytes() {
    let b = base(0);
    let mut h = Store::open(&b.holder).unwrap();
    b.take_v2(&mut h).unwrap();
    let end = h.log_end();
    let refused = |r: Result<Vec<Hash>, StoreError>| {
        assert!(
            matches!(r, Err(StoreError::Redacted { request }) if request == b.request()),
            "{r:?}"
        );
    };
    refused(h.write(vec![(kinds::BLOCK, enc(&b.secret))]));
    refused(h.write(vec![(
        kinds::CHANGE,
        enc(&change("again", creates(&[&b.secret]))),
    )]));
    assert_eq!(h.log_end(), end);
    assert!(!holds_canary(&b.holder));
    // a later take carries the removed object only as its dropped identity
    let again = b.take_v2(&mut h).unwrap();
    assert_eq!((again.new_objects, again.frames), (0, 0));
    let v1 = b.take_v1(&mut h);
    assert_eq!((v1.new_objects, v1.frames), (0, 0));
    assert!(!holds_canary(&b.holder));
}

#[test]
fn ha11_pending_erasure_recovers_after_seal() {
    let b = base(0);
    let mut h = Store::open(&b.holder).unwrap();
    let files = b.file(b.v2.0, "v2");
    arm_failpoint(Some((Failpoint::AfterApplySync, FailAction::Fail)));
    let e = take(&mut h, &files, "v2", b.v2.1).unwrap_err();
    arm_failpoint(None);
    assert!(
        matches!(e, TakeError::Store(StoreError::RedactionPending)),
        "{e:?}"
    );
    // sealed, not finished: no read is served, and the batch is not rolled back
    let end = h.log_end();
    assert!(matches!(
        h.get_ref("tag/v2"),
        Ok(_) | Err(StoreError::RedactionPending)
    ));
    assert!(matches!(
        h.reader()
            .for_each_record_from(ORIGIN, end, |_, _, _| Ok(())),
        Err(StoreError::RedactionPending)
    ));
    assert!(holds_canary(&b.holder));
    let notice = rdy_store::drop_unclean_for_tests(h);
    assert!(
        notice.contains("(Content removal was authorized but has not finished.)"),
        "{notice}"
    );
    // the next open finishes the exact authorized removal
    let mut h = Store::open(&b.holder).unwrap();
    assert_eq!(h.log_end(), end);
    assert!(!holds_canary(&b.holder));
    assert_eq!(h.get_ref("tag/v2").unwrap(), Some(b.v2.0.0.to_vec()));
    assert!(copies(&h, b.secret_key()).iter().all(|(_, d)| *d));
    // the repeat is complete already: nothing more is written
    let again = b.take_v2(&mut h).unwrap();
    assert_eq!(
        (
            again.new_objects,
            again.frames,
            again.redacted_objects,
            again.end
        ),
        (0, 0, 0, end)
    );
}

#[test]
fn ha11_receiver_scan_is_bounded() {
    assert_eq!(
        (MAX_RECEIVER_FRAMES, MAX_RECEIVER_BYTES, MAX_RECEIVER_SPANS),
        (1_000_000, 2_147_483_648, 1_000_000)
    );
    assert_eq!(ReceiverCaps::current(), ReceiverCaps::DEFAULT);
    let caps = ReceiverCaps::DEFAULT;
    assert_eq!(
        (caps.frames_label(), caps.bytes_label(), caps.spans_label()),
        (
            "1000000 receiver frames".to_string(),
            "2147483648 receiver bytes".to_string(),
            "1000000 receiver spans".to_string()
        )
    );
    // a holder with no approval before or after the take scans nothing: the caps do not apply
    {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        drop(Store::create(&src).unwrap());
        let (_, l) = land(&src, "work", creates(&[&block("b-00001", "plain")]));
        let t = tag(&src, "v1", l);
        let repo = Store::open(&src).unwrap().repo_id().unwrap().unwrap();
        let mut h = Store::create_holder(&dir.path().join("h"), repo).unwrap();
        set_receiver_caps_for_tests(Some(ReceiverCaps {
            frames: 1,
            bytes: 1,
            spans: 0,
        }));
        let got = take(&mut h, &upload(&src, t, &dir.path().join("v1")), "v1", l);
        set_receiver_caps_for_tests(None);
        assert!(got.unwrap().new_objects > 0);
    }
    let b = base(0);
    let ready = b.dir.path().join("ready");
    copy_dir(&b.holder, &ready);
    // what the removal's take makes: every frame, payload byte and span of the log after it
    let (frames_after, bytes_after, spans_after, marker_len) = {
        let mut h = Store::open(&b.holder).unwrap();
        b.take_v2(&mut h).unwrap();
        let all = frames(&h);
        let spans = all.iter().filter(|f| f.3).count() as u64;
        let mut bytes = 0u64;
        let mut last_marker = 0u64;
        for seg in segment_bytes(&b.holder).values() {
            let mut at = 0usize;
            while at + 37 <= seg.len() {
                let len = u32::from_le_bytes(seg[at..at + 4].try_into().unwrap()) as u64;
                bytes += len;
                if seg[at + 4] == kinds::BATCH_COMMIT {
                    last_marker = len;
                }
                at += 37 + len as usize;
            }
        }
        (all.len() as u64, bytes, spans, last_marker)
    };
    // the take bounds the marker at its 64-byte bound
    let bytes_bound = bytes_after - marker_len + 64;
    let attempt = |caps: ReceiverCaps| {
        let dir = b
            .dir
            .path()
            .join(format!("try-{}-{}-{}", caps.frames, caps.bytes, caps.spans));
        copy_dir(&ready, &dir);
        let mut h = Store::open(&dir).unwrap();
        set_receiver_caps_for_tests(Some(caps));
        let got = b.take_v2(&mut h);
        let reopened = got.is_ok().then(|| {
            drop(h);
            Store::open(&dir).map(|_| ())
        });
        set_receiver_caps_for_tests(None);
        (got, reopened, dir)
    };
    let exact = ReceiverCaps {
        frames: frames_after,
        bytes: bytes_bound,
        spans: spans_after,
    };
    // N passes, and the store it makes reopens under the same caps
    let (got, reopened, _) = attempt(exact);
    got.unwrap();
    reopened.unwrap().unwrap();
    // N+1 of each is refused before anything is written
    for (caps, label) in [
        (
            ReceiverCaps {
                frames: frames_after - 1,
                ..exact
            },
            format!("{} receiver frames", frames_after - 1),
        ),
        (
            ReceiverCaps {
                bytes: bytes_bound - 1,
                ..exact
            },
            format!("{} receiver bytes", bytes_bound - 1),
        ),
        (
            ReceiverCaps {
                spans: spans_after - 1,
                ..exact
            },
            format!("{} receiver spans", spans_after - 1),
        ),
    ] {
        let (got, _, dir) = attempt(caps);
        let e = match got.unwrap_err() {
            TakeError::Stream(e) => e,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            (e.code, e.message),
            ("E_LIMIT", format!("stream exceeds {label}"))
        );
        assert_eq!(e.fix, "reduce the stream to the stated limit");
        assert_eq!(segment_bytes(&dir), segment_bytes(&ready));
    }
    // an open over the caps fails closed
    for caps in [
        ReceiverCaps {
            frames: frames_after - 1,
            ..exact
        },
        ReceiverCaps {
            bytes: bytes_after - 1,
            ..exact
        },
        ReceiverCaps {
            spans: spans_after - 1,
            ..exact
        },
    ] {
        set_receiver_caps_for_tests(Some(caps));
        let opened = Store::open(&b.holder);
        set_receiver_caps_for_tests(None);
        assert!(
            matches!(opened, Err(StoreError::RedactionLimit { .. })),
            "{caps:?}: {:?}",
            opened.err()
        );
    }
    drop(Store::open(&b.holder).unwrap());
}

/// Records as kind and bytes.
type Items = Vec<(u8, Vec<u8>)>;

/// An edit of the secret's span, returning the extra records it needs.
type SpanEdit = fn(&mut rdy_model::redaction::DropSpan) -> Items;

/// A second removal of the base's secret version that is consistent in itself but not with the
/// first: the source's request with `edit` applied to its block span (`edit` may return extra
/// records, such as a residue), a landing that names it, its apply and a tag `v3` of that
/// landing, with the block as a dropped record of that apply. Returns the batch file, the tag's
/// landing, and the request, landing and apply records (with the extras first).
fn conflicting(b: &Base, name: &str, edit: SpanEdit) -> (Vec<File>, Hash, Items) {
    use rdy_model::redaction::RedactionApply;
    use rdy_store::sync_stream::StreamRecord;
    let secret = b.secret_key();
    let mut request = b.fixture.requests[0].draft.request.clone();
    let span = request
        .manifest
        .spans
        .iter_mut()
        .find(|s| s.kind == secret.kind && s.hash == secret.hash)
        .unwrap();
    let mut records = edit(span);
    let extras = records.len();
    let (len, residue) = (span.len, span.residue);
    let mut buf = vec![0u8; rdy_model::redaction::MAX_RECORD_BYTES];
    let report_hash = request.manifest.report_hash(&mut buf).unwrap();
    let request_bytes = enc(&request);
    let rid = Hash::of(&request_bytes);
    let mut landing = b.fixture.landing_record.clone().unwrap();
    landing.checks = vec![rid];
    let (landing_kind, landing_bytes) = rdy_store::codec::item(kinds::LANDING, &landing).unwrap();
    let lid = Hash::of(&landing_bytes);
    let apply_bytes = enc(&RedactionApply {
        request: rid,
        landing: lid,
        report_hash,
    });
    let aid = Hash::of(&apply_bytes);
    let tag_bytes = enc(&Tag {
        name: "v3".into(),
        landing: lid,
        message: None,
        tagged_by: prov("human:alice", Role::Approver),
    });
    let tid = Hash::of(&tag_bytes);
    records.extend([
        (kinds::REDACTION, request_bytes),
        (landing_kind, landing_bytes),
        (kinds::REDACTION_APPLY, apply_bytes),
    ]);
    // the batch, ending with the tag; its prerequisites are every key a record names that the
    // batch does not hold
    let mut source: BTreeMap<ObjectKey, StreamRecord> = BTreeMap::new();
    let mut order = vec![];
    let mut prerequisites = BTreeSet::new();
    let present: Vec<(u8, Vec<u8>)> = records
        .iter()
        .cloned()
        .chain([(kinds::TAG, tag_bytes)])
        .collect();
    let in_batch: BTreeSet<ObjectKey> = present
        .iter()
        .map(|(k, p)| key(rdy_store::codec::logical_kind(*k), Hash::of(p)))
        .chain([secret])
        .collect();
    for (kind, payload) in &present {
        for r in rdy_store::object_refs::refs_of(*kind, payload).unwrap() {
            match r.kind {
                Some(k) if !in_batch.contains(&key(k, r.hash)) => {
                    prerequisites.insert(key(k, r.hash));
                }
                Some(_) => {}
                None => assert!(in_batch.iter().any(|k| k.hash == r.hash)),
            }
        }
    }
    if let Some(r) = residue
        && !in_batch.contains(&key(kinds::REDACTION_RESIDUE, r))
    {
        prerequisites.insert(key(kinds::REDACTION_RESIDUE, r));
    }
    // the extras, the dropped block (it names its residue, and the request names it), then the
    // request, landing and apply
    let mut put = |k: ObjectKey, r: StreamRecord| {
        order.push(k);
        source.insert(k, r);
    };
    // a record by its logical kind, with the encoding its raw frame kind gives
    let present_record = |kind: u8, payload: &Vec<u8>| {
        let k = key(rdy_store::codec::logical_kind(kind), Hash::of(payload));
        (
            k,
            StreamRecord::Present {
                encoding: rdy_store::codec::split(kind, payload).unwrap().version,
                key: k,
                payload: payload.clone(),
            },
        )
    };
    for (kind, payload) in &present[..extras] {
        let (k, r) = present_record(*kind, payload);
        put(k, r);
    }
    put(
        secret,
        StreamRecord::Dropped {
            encoding: 0,
            key: secret,
            original_len: len,
            apply: aid,
            residue,
        },
    );
    for (kind, payload) in &present[extras..] {
        let (k, r) = present_record(*kind, payload);
        put(k, r);
    }
    let repo = Store::open(&b.src).unwrap().repo_id().unwrap().unwrap();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        repo,
        tid,
        prerequisites.into_iter().collect(),
    );
    let out = b.dir.path().join(format!("{name}.rdysync"));
    write_batch(&mut source, &header, &order, File::create(&out).unwrap()).unwrap();
    (vec![File::open(&out).unwrap()], lid, records)
}

/// The secret span's length, one more than its frame's.
fn longer(s: &mut rdy_model::redaction::DropSpan) -> Items {
    s.len += 1;
    vec![]
}

/// The secret span's residue, another residue of the same version (another order), stored.
fn other_residue(s: &mut rdy_model::redaction::DropSpan) -> Items {
    let block = block("b-00001", CANARY);
    let mut res = rdy_model::redaction::BlockResidue::of(&block);
    res.order = "W".into();
    let frame = residue_frame(&rdy_model::redaction::RedactionResidue::Block(res));
    s.residue = Some(Hash::of(&frame.1));
    vec![frame]
}

#[test]
fn ha11_conflicting_tuple_of_a_removed_key_refuses_the_take() {
    let b = base(0);
    let mut h = Store::open(&b.holder).unwrap();
    b.take_v2(&mut h).unwrap();
    assert!(copies(&h, b.secret_key()).iter().all(|(_, d)| *d));
    for (name, edit) in [("longer", longer as SpanEdit), ("residue", other_residue)] {
        let (files, landing, _) = conflicting(&b, name, edit);
        let before = (segment_bytes(&b.holder), h.log_end());
        let e = match take(&mut h, &files, "v3", landing) {
            Err(TakeError::Stream(e)) => e,
            other => panic!("{name}: {other:?}"),
        };
        assert_eq!(
            (e.code, e.message.as_str(), e.object),
            (
                "E_BAD_RECORD",
                "stream record 0 at byte 0: invalid removal proof",
                Some(b.secret_key())
            ),
            "{name}"
        );
        assert_eq!((segment_bytes(&b.holder), h.log_end()), before, "{name}");
        assert_eq!(h.get_ref("tag/v3").unwrap(), None, "{name}");
    }
    drop(h);
    drop(Store::open(&b.holder).unwrap());
}

#[test]
fn ha11_conflicting_tuple_of_a_removed_key_refuses_the_open() {
    for (name, edit) in [("longer", longer as SpanEdit), ("residue", other_residue)] {
        let b = base(0);
        let mut h = Store::open(&b.holder).unwrap();
        b.take_v2(&mut h).unwrap();
        drop(h);
        // the conflicting removal's records reach the holder's log by a raw write
        let (_, _, records) = conflicting(&b, name, edit);
        let at = append_raw(&b.holder, &records);
        let apply = *at.last().unwrap();
        match Store::open(&b.holder) {
            Err(StoreError::RedactionCorrupt { segment, offset }) => {
                assert_eq!(Location { segment, offset }, apply, "{name}")
            }
            other => panic!("{name}: {:?}", other.err()),
        }
    }
}

#[test]
fn ha11_complete_retry_fits_the_receiver_caps() {
    let b = base(0);
    let mut h = Store::open(&b.holder).unwrap();
    b.take_v2(&mut h).unwrap();
    let end = h.log_end();
    // the log after the removal's take: its frames, payload bytes and spans
    let all = frames(&h);
    let spans = all.iter().filter(|f| f.3).count() as u64;
    let mut bytes = 0u64;
    for seg in segment_bytes(&b.holder).values() {
        let mut at = 0usize;
        while at + 37 <= seg.len() {
            let len = u32::from_le_bytes(seg[at..at + 4].try_into().unwrap()) as u64;
            bytes += len;
            at += 37 + len as usize;
        }
    }
    let exact = ReceiverCaps {
        frames: all.len() as u64,
        bytes,
        spans,
    };
    let retry = |h: &mut Store, caps: ReceiverCaps| {
        set_receiver_caps_for_tests(Some(caps));
        let got = b.take_v2(h);
        set_receiver_caps_for_tests(None);
        got
    };
    // the log at exactly each cap: the complete retry writes nothing and returns the log's end
    let before = segment_bytes(&b.holder);
    let got = retry(&mut h, exact).unwrap();
    assert_eq!(
        (got.new_objects, got.frames, got.redacted_objects, got.end),
        (0, 0, 0, end)
    );
    assert_eq!(segment_bytes(&b.holder), before);
    // one under any cap: the existing log is still checked against it
    for (caps, label) in [
        (
            ReceiverCaps {
                frames: exact.frames - 1,
                ..exact
            },
            format!("{} receiver frames", exact.frames - 1),
        ),
        (
            ReceiverCaps {
                bytes: exact.bytes - 1,
                ..exact
            },
            format!("{} receiver bytes", exact.bytes - 1),
        ),
        (
            ReceiverCaps {
                spans: exact.spans - 1,
                ..exact
            },
            format!("{} receiver spans", exact.spans - 1),
        ),
    ] {
        let e = match retry(&mut h, caps) {
            Err(TakeError::Stream(e)) => e,
            other => panic!("{caps:?}: {other:?}"),
        };
        assert_eq!(
            (e.code, e.message),
            ("E_LIMIT", format!("stream exceeds {label}"))
        );
        assert_eq!(segment_bytes(&b.holder), before);
    }
}
