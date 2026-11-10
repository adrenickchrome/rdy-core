//! rpl24 (repo links, D-CN4QJ): a cited project's portable removal proof is an erasure authority
//! in a citing project's foreign copy. `Store::take_foreign_removal` checks the proof against the
//! cited repo id, as a holder's take does, stores the proof's objects outside canon, seals one
//! `Removal` entry naming the proof and the versions, and erases those versions' foreign
//! occurrences only: on live and revoked links alike, never local content of the same hash. A
//! version the store does not hold is reported and skipped. The open, fsck, compaction and a
//! follower apply the same authority; a bad proof or a proof of another repo writes nothing.
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource};
use rdy_store::dump::{DumpHeader, DumpItem, DumpReader, DumpWriter, RemovalTally};
use rdy_store::dump_chain::{BatchMarker, ChainEnd, DumpChainHeader};
use rdy_store::follower::{self, FollowerMark};
use rdy_store::foreign_removal::{ForeignProof, ForeignRemovalError, ForeignRemovalTaken};
use rdy_store::format::DumpStore;
use rdy_store::ingest::Ingest;
use rdy_store::object_refs::ObjectKey;
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
use rdy_store::segment::Location;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::StreamRecord;
use rdy_store::{Store, StoreError, StoreLock, StoreReader, codec, kinds};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The bytes of a log frame's header.
const HEADER: u64 = 37;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn block(id_: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: Provenance {
            actor: "agent:w".into(),
            run: Some("w1".into()),
            role: Role::Writer,
            at_ms: 10,
        },
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// The BLOCK version of `b`: the hash of its frame.
fn version_of(b: &Block) -> Hash {
    Hash::of(&codec::item_new(kinds::BLOCK, b).1)
}

fn pair(b: &Block) -> (BlockId, Hash) {
    (b.id.clone(), version_of(b))
}

fn fv(b: &Block) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: version_of(b),
        content: b.content_hash(),
        encoding: 0,
        derived: ForeignDerived::default(),
    }
}

fn link(link: &str) -> ForeignSource {
    ForeignSource::Link {
        link: link.into(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    }
}

/// One link source of the cited repo: its newest entry and its selection.
struct Source {
    repo: RepoId,
    source: ForeignSource,
    head: Option<Hash>,
    sel: BTreeMap<BlockId, Hash>,
}

impl Source {
    fn new(repo: RepoId, source: ForeignSource) -> Source {
        Source {
            repo,
            source,
            head: None,
            sel: BTreeMap::new(),
        }
    }

    /// Delivers `added` to `s` as one sealed entry with its new blocks.
    fn deliver(&mut self, s: &mut Store, added: &[&Block]) -> Hash {
        for b in added {
            self.sel.insert(b.id.clone(), version_of(b));
        }
        let e = ForeignEntry {
            repo: self.repo,
            source: self.source.clone(),
            event: ForeignEvent::Delivery {
                prev: self.head,
                publication: ForeignPublication {
                    tag: "v1".into(),
                    profile: "decisions".into(),
                    profile_version: None,
                },
                set: None,
                added: added.iter().map(|b| fv(b)).collect(),
                changed: vec![],
                removed: vec![],
                shells: vec![],
                selection: selection_identity(self.sel.iter()).unwrap(),
            },
        };
        let blocks: Vec<Block> = added.iter().map(|b| (*b).clone()).collect();
        let h = s.put_foreign(&e, &blocks).unwrap();
        self.head = Some(h);
        h
    }

    fn revoke(&mut self, s: &mut Store) {
        let e = ForeignEntry {
            repo: self.repo,
            source: self.source.clone(),
            event: ForeignEvent::Revoked {
                prev: self.head.unwrap(),
            },
        };
        self.head = Some(s.put_foreign(&e, &[]).unwrap());
    }
}

/// The cited project: five blocks, one change that creates them, and one applied removal of
/// `v1`, `v2`, `v4` and `v5` that a person approved. `v3` stays.
struct Cited {
    _dir: tempfile::TempDir,
    store: Store,
    reader: StoreReader,
    repo: RepoId,
    apply: Hash,
    v1: Block,
    v2: Block,
    v3: Block,
    v4: Block,
    v5: Block,
}

impl Cited {
    fn new() -> Cited {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cited");
        let v1 = block("D-00001", "a leaked key in the first decision");
        let v2 = block("D-00002", "a leaked key in the second decision");
        let v3 = block("D-00003", "a decision that stays");
        let v4 = block("D-00004", "removed, never delivered");
        let v5 = block("D-00005", "removed, and the citing project wrote it too");
        let repo = {
            let mut s = Store::create(&path).unwrap();
            s.commit_to_view(
                "work",
                change(
                    "work",
                    [&v1, &v2, &v3, &v4, &v5]
                        .into_iter()
                        .map(|b| Op::Create { block: b.clone() })
                        .collect(),
                ),
            )
            .unwrap();
            s.repo_id().unwrap().unwrap()
        };
        let targets = [&v1, &v2, &v4, &v5]
            .into_iter()
            .map(|b| RedactTarget::Version {
                id: b.id.clone(),
                version: version_of(b),
            })
            .collect();
        let fixture = authorized_batch_fixture(
            &path,
            &[RequestSpec::new("work", targets)],
            &FixtureOptions::approved(Proof::local_human()),
        );
        let apply = fixture.applies[0].0;
        // the cited project's own open finishes its removal
        let store = Store::open(&path).unwrap();
        assert!(removed(&store, &v1));
        Cited {
            _dir: dir,
            reader: store.reader(),
            store,
            repo,
            apply,
            v1,
            v2,
            v3,
            v4,
            v5,
        }
    }

    /// The cited project's records as a proof source.
    fn source(&self) -> StoreSource<'_> {
        StoreSource {
            reader: &self.reader,
            end: END_OF_LOG,
        }
    }

    /// Every proof record of the cited project's apply, by key (what a stream would carry).
    fn records(&self) -> BTreeMap<ObjectKey, StreamRecord> {
        let mut src = self.source();
        let mut out = BTreeMap::new();
        for kind in [
            kinds::REDACTION_APPLY,
            kinds::LANDING,
            kinds::REDACTION,
            kinds::CHANGE,
            kinds::PROPOSAL,
            kinds::REDACTION_RESIDUE,
            kinds::BLOCK,
        ] {
            let mut hashes = vec![];
            self.store
                .reader()
                .for_each_object_of(kind, |h| {
                    hashes.push(h);
                    Ok(())
                })
                .unwrap();
            for h in hashes {
                let k = ObjectKey { kind, hash: h };
                if let Some(r) = src.record(k).unwrap() {
                    out.insert(k, r);
                }
            }
        }
        out
    }
}

/// The citing project: its own block, a live link that delivers `v1`, `v3` and `v5`, and a
/// revoked link that delivered `v2`. `local_v5` decides whether it also writes `v5` itself,
/// after the delivery (one frame, the delivery's).
struct Citing {
    _dir: tempfile::TempDir,
    path: PathBuf,
    store: Store,
    own: Block,
}

fn citing(c: &Cited, local_v5: bool) -> Citing {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("citing");
    let mut s = Store::create(&path).unwrap();
    let own = block("C-00001", "a local claim");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: own.clone() }]),
    )
    .unwrap();
    let mut live = Source::new(c.repo, link("rl-00001"));
    live.deliver(&mut s, &[&c.v1, &c.v3, &c.v5]);
    let mut gone = Source::new(c.repo, link("rl-00002"));
    gone.deliver(&mut s, &[&c.v2]);
    gone.revoke(&mut s);
    if local_v5 {
        s.commit_to_view(
            "work",
            change(
                "work",
                vec![Op::Create {
                    block: c.v5.clone(),
                }],
            ),
        )
        .unwrap();
    }
    Citing {
        _dir: dir,
        path,
        store: s,
        own,
    }
}

fn take(
    s: &mut Store,
    claimed: RepoId,
    proof_repo: RepoId,
    apply: Hash,
    source: &mut dyn ObjectSource,
) -> Result<ForeignRemovalTaken, ForeignRemovalError> {
    s.take_foreign_removal(
        &claimed,
        ForeignProof {
            repo: proof_repo,
            apply,
            source,
        },
    )
}

/// The bytes of every segment file of the store at `dir`, by file name.
fn segments(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir.join("segments")).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("seg-") && name.ends_with(".log") {
            out.insert(name, std::fs::read(e.path()).unwrap());
        }
    }
    out
}

/// Every frame of the log at `dir` before `end`: its place, logical kind, hash and payload.
fn frames(dir: &Path) -> Vec<(Location, u8, Hash, Vec<u8>)> {
    let mut out = vec![];
    for (seg, bytes) in segments(dir).values().enumerate() {
        let mut at = 0usize;
        while at + HEADER as usize <= bytes.len() {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let kind = codec::logical_kind(bytes[at + 4]);
            let hash = Hash(bytes[at + 5..at + 37].try_into().unwrap());
            let payload = bytes[at + 37..at + 37 + len].to_vec();
            out.push((
                Location {
                    segment: seg as u32,
                    offset: at as u64,
                },
                kind,
                hash,
                payload,
            ));
            at += 37 + len;
        }
    }
    out
}

/// The payloads of every BLOCK frame of `version` in the log at `dir`.
fn block_payloads(dir: &Path, version: Hash) -> Vec<Vec<u8>> {
    frames(dir)
        .into_iter()
        .filter(|(_, k, h, _)| *k == kinds::BLOCK && *h == version)
        .map(|(_, _, _, p)| p)
        .collect()
}

fn erased(dir: &Path, b: &Block) -> bool {
    let p = block_payloads(dir, version_of(b));
    !p.is_empty() && p.iter().all(|p| p.iter().all(|x| *x == 0))
}

fn intact(dir: &Path, b: &Block) -> bool {
    let p = block_payloads(dir, version_of(b));
    !p.is_empty() && p.iter().all(|p| Hash::of(p) == version_of(b))
}

/// Whether the BLOCK of `b` reads as removed in `s`.
fn removed(s: &Store, b: &Block) -> bool {
    matches!(
        s.get_kind_raw(kinds::BLOCK, &version_of(b)),
        Err(StoreError::Redacted { .. })
    )
}

/// fsck passes on the store's index and on a rebuilt one, with `entries` foreign entries.
fn clean(s: &mut Store, entries: usize) {
    assert_eq!(s.reader().check_objects().unwrap().foreign, entries);
    s.reindex().unwrap();
    assert_eq!(s.reader().check_objects().unwrap().foreign, entries);
}

#[test]
fn rpl24_verified_removal_authorizes_only_foreign_occurrences() {
    let c = Cited::new();
    let mut x = citing(&c, true);
    let canon = x.store.canon_head().unwrap();
    let views = x.store.reader().refs_with_prefix("view/").unwrap();
    let mut src = c.source();
    let taken = take(&mut x.store, c.repo, c.repo, c.apply, &mut src).unwrap();

    // live and revoked links alike; a local version is kept; an undelivered one is skipped
    assert_eq!(taken.erased, vec![pair(&c.v1), pair(&c.v2)]);
    assert_eq!(taken.kept_local, vec![pair(&c.v5)]);
    assert_eq!(taken.skipped, vec![pair(&c.v4)]);
    assert!(taken.already_removed.is_empty());
    let entry = taken.entry.expect("a Removal entry");
    let e = x.store.foreign_entry(&entry).unwrap();
    assert_eq!(e.repo, c.repo);
    assert_eq!(
        e.event,
        ForeignEvent::Removal {
            proof: c.apply,
            versions: vec![pair(&c.v1), pair(&c.v2)],
        }
    );
    assert_eq!(taken.counts.frames, 2);

    // the foreign occurrences are zeros and read as removed; everything else is intact
    for b in [&c.v1, &c.v2] {
        assert!(erased(&x.path, b), "{}", b.id);
        assert!(removed(&x.store, b));
    }
    for b in [&c.v3, &c.v5, &x.own] {
        assert!(intact(&x.path, b), "{}", b.id);
        assert_eq!(x.store.get_block(&version_of(b)).unwrap().id, b.id);
    }
    // the proof is outside canon: no landing, view or canon move
    assert_eq!(x.store.canon_head().unwrap(), canon);
    assert_eq!(x.store.reader().refs_with_prefix("view/").unwrap(), views);
    assert!(x.store.has_kind(kinds::REDACTION_APPLY, &c.apply).unwrap());
    // 4 entries: two deliveries, a revocation and the removal
    clean(&mut x.store, 4);

    // the open derives the same authority from the log alone
    x.store.close().unwrap();
    let mut s = Store::open(&x.path).unwrap();
    assert!(erased(&x.path, &c.v1) && erased(&x.path, &c.v2));
    assert!(intact(&x.path, &c.v5) && intact(&x.path, &c.v3));
    clean(&mut s, 4);

    // a second take of the same proof writes nothing
    let end = s.log_end();
    let again = take(&mut s, c.repo, c.repo, c.apply, &mut c.source()).unwrap();
    assert_eq!(again.entry, None);
    assert!(again.erased.is_empty());
    assert_eq!(again.already_removed, vec![pair(&c.v1), pair(&c.v2)]);
    assert_eq!(s.log_end(), end);
}

#[test]
fn rpl24_local_same_hash_survives() {
    let c = Cited::new();

    // v5 written locally after its delivery: one frame, the delivery's, which local content
    // now needs
    let mut x = citing(&c, true);
    let local = x.store.reader().view_head("work").unwrap();
    take(&mut x.store, c.repo, c.repo, c.apply, &mut c.source()).unwrap();
    assert!(intact(&x.path, &c.v5));
    assert_eq!(x.store.get_block(&version_of(&c.v5)).unwrap(), c.v5);
    assert_eq!(x.store.reader().view_head("work").unwrap(), local);
    clean(&mut x.store, 4);

    // v5 written locally before its delivery: the frame is local, and the delivery holds it
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("citing");
    let mut s = Store::create(&path).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: c.v5.clone(),
            }],
        ),
    )
    .unwrap();
    let mut live = Source::new(c.repo, link("rl-00001"));
    live.deliver(&mut s, &[&c.v5]);
    // every approved version it holds is local: nothing is erased, so nothing is written
    let end = s.log_end();
    let taken = take(&mut s, c.repo, c.repo, c.apply, &mut c.source()).unwrap();
    assert_eq!(taken.entry, None);
    assert!(taken.erased.is_empty());
    assert_eq!(taken.kept_local, vec![pair(&c.v5)]);
    assert_eq!(taken.skipped, vec![pair(&c.v1), pair(&c.v2), pair(&c.v4)]);
    assert_eq!(s.log_end(), end);
    assert!(intact(&path, &c.v5));
    clean(&mut s, 1);
}

/// A full dump of `s` (a follower's seed) and its chain end.
fn full_dump(s: &Store) -> (Vec<u8>, ChainEnd) {
    let r = s.reader();
    let end = s.log_end();
    let repo = r.repo_id().unwrap().unwrap();
    let end_batch = r.batch_marker_at(end).unwrap().unwrap_or(BatchMarker::ZERO);
    let store = DumpStore::Features(r.store_features().unwrap());
    let header = DumpHeader {
        store: store.clone(),
        log_end: end,
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(repo, end_batch)),
    };
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    let mut tally = RemovalTally::default();
    r.for_each_batch_after(ORIGIN, end, &mut |b| {
        tally.observe(&b)?;
        w.batch(&b)
    })
    .unwrap();
    let facts = tally.resolved(&r, end).unwrap();
    let (bytes, totals) = w.finish(facts).unwrap();
    (
        bytes,
        ChainEnd {
            repo_id: repo,
            store_format: store,
            end,
            end_batch,
            digest: totals.digest,
        },
    )
}

/// Loads `bytes` into a new store at `to` and marks it a follower at `chain`.
fn seed_follower(bytes: &[u8], chain: &ChainEnd, to: &Path) {
    let (mut reader, _, header) = DumpReader::open(bytes).unwrap();
    let mut ingest = Ingest::create(to).unwrap();
    let mut n = 0;
    while let Some(item) = reader.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            n += 1;
            ingest.batch(n, &b).unwrap();
        }
    }
    ingest.upgrade(&header.store).unwrap();
    ingest.finish().unwrap();
    let lock = StoreLock::take(to).unwrap();
    follower::write_seed_mark(
        to,
        &lock,
        &FollowerMark {
            chain: chain.clone(),
            files: vec![],
        },
    )
    .unwrap();
}

#[test]
fn rpl24_fsck_compact_and_follower_honor_removal() {
    let c = Cited::new();

    // a follower seeded before the removal holds every foreign copy intact
    let mut x = citing(&c, true);
    let (bytes, chain) = full_dump(&x.store);
    let fdir = x._dir.path().join("follower");
    seed_follower(&bytes, &chain, &fdir);
    assert!(intact(&fdir, &c.v1) && intact(&fdir, &c.v2));

    let taken = take(&mut x.store, c.repo, c.repo, c.apply, &mut c.source()).unwrap();
    assert_eq!(taken.erased.len(), 2);

    // fsck accepts the zeros the Removal names, on both indexes
    clean(&mut x.store, 4);

    // compaction counts and punches them, and moves no byte rdy reads
    let report = x.store.log_bytes().unwrap();
    let removed_bytes: u64 = [&c.v1, &c.v2]
        .iter()
        .map(|b| codec::item_new(kinds::BLOCK, *b).1.len() as u64)
        .sum();
    assert_eq!(report.removed_bytes, removed_bytes);
    let block_row = report
        .by_kind
        .iter()
        .find(|k| k.kind == kinds::BLOCK)
        .unwrap();
    assert_eq!(block_row.removed_bytes, removed_bytes);
    let before = segments(&x.path);
    let compacted = x.store.compact(false).unwrap();
    assert_eq!(compacted.removed_bytes, removed_bytes);
    assert_eq!(segments(&x.path), before);
    clean(&mut x.store, 4);

    // the follower: the source's removal batch reaches its log (an interrupted follow, synced
    // before its erasure); its open erases the same occurrences, byte for byte
    let end = x.store.log_end();
    assert_eq!(end.segment, chain.end.segment, "one segment");
    let seg = format!("seg-{:06}.log", end.segment);
    let tail = segments(&x.path)[&seg][chain.end.offset as usize..end.offset as usize].to_vec();
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(fdir.join("segments").join(&seg))
            .unwrap();
        f.write_all(&tail).unwrap();
        f.sync_all().unwrap();
    }
    assert!(intact(&fdir, &c.v1), "nothing erased before the open");
    let f = Store::open_follower(&fdir).unwrap();
    assert_eq!(segments(&fdir), segments(&x.path));
    assert!(erased(&fdir, &c.v1) && erased(&fdir, &c.v2));
    assert!(intact(&fdir, &c.v5) && intact(&fdir, &c.v3));
    assert_eq!(f.reader().check_objects().unwrap().foreign, 4);
    assert!(removed(&f, &c.v2));
    drop(f);

    // a crash after the removal is sealed and before its erasure: the next open erases
    let mut y = citing(&c, true);
    arm_failpoint(Some((Failpoint::AfterApplySync, FailAction::Fail)));
    let failed = take(&mut y.store, c.repo, c.repo, c.apply, &mut c.source());
    arm_failpoint(None);
    assert!(
        matches!(
            failed,
            Err(ForeignRemovalError::Store(StoreError::RedactionPending))
        ),
        "{failed:?}"
    );
    assert!(intact(&y.path, &c.v1) && intact(&y.path, &c.v2));
    drop(y.store);
    let mut s = Store::open(&y.path).unwrap();
    assert!(erased(&y.path, &c.v1) && erased(&y.path, &c.v2));
    assert!(intact(&y.path, &c.v5));
    clean(&mut s, 4);
}

#[test]
fn rpl24_bad_proof_and_wrong_repo_write_nothing() {
    let c = Cited::new();
    let mut x = citing(&c, false);
    let before = segments(&x.path);
    let end = x.store.log_end();
    let unchanged = |x: &Citing| {
        assert_eq!(x.store.log_end(), end);
        assert_eq!(segments(&x.path), before);
        assert!(intact(&x.path, &c.v1) && intact(&x.path, &c.v2));
    };

    // a proof of another repo
    let other = RepoId([9; 16]);
    match take(&mut x.store, c.repo, other, c.apply, &mut c.source()) {
        Err(ForeignRemovalError::RepoMismatch { source, target }) => {
            assert_eq!((source, target), (other, c.repo));
        }
        other => panic!("not a repo mismatch: {other:?}"),
    }
    unchanged(&x);

    // the project's own repo
    let own = x.store.repo_id().unwrap().unwrap();
    assert!(matches!(
        take(&mut x.store, own, own, c.apply, &mut c.source()),
        Err(ForeignRemovalError::Store(StoreError::Refused(_)))
    ));
    unchanged(&x);

    // a proof whose apply the stream lacks
    let mut records = c.records();
    match take(&mut x.store, c.repo, c.repo, Hash([3; 32]), &mut records) {
        Err(ForeignRemovalError::Proof(e)) => assert_eq!(e.code, "E_UNRESOLVED_REF"),
        other => panic!("not an unresolved proof: {other:?}"),
    }
    unchanged(&x);

    // a proof whose landing no person approved
    let apply_key = ObjectKey {
        kind: kinds::REDACTION_APPLY,
        hash: c.apply,
    };
    let StreamRecord::Present { payload, .. } = &records[&apply_key] else {
        panic!("a present apply");
    };
    let mut apply: rdy_model::redaction::RedactionApply =
        codec::decode_record(kinds::REDACTION_APPLY, payload).unwrap();
    let mut landing: LandingRecord = c.store.landing(&apply.landing).unwrap();
    landing.approved_by.role = Role::Writer;
    landing.read_identity = None;
    let (raw, bytes) = codec::item(kinds::LANDING, &landing).unwrap();
    let encoding = codec::split(raw, &bytes).unwrap().version;
    let lkey = ObjectKey {
        kind: kinds::LANDING,
        hash: Hash::of(&bytes),
    };
    records.insert(
        lkey,
        StreamRecord::Present {
            key: lkey,
            encoding,
            payload: bytes,
        },
    );
    apply.landing = lkey.hash;
    let (araw, abytes) = codec::item(kinds::REDACTION_APPLY, &apply).unwrap();
    let akey = ObjectKey {
        kind: kinds::REDACTION_APPLY,
        hash: Hash::of(&abytes),
    };
    records.insert(
        akey,
        StreamRecord::Present {
            key: akey,
            encoding: codec::split(araw, &abytes).unwrap().version,
            payload: abytes,
        },
    );
    match take(&mut x.store, c.repo, c.repo, akey.hash, &mut records) {
        Err(ForeignRemovalError::Proof(e)) => {
            assert_eq!(e.code, "E_BAD_RECORD");
            assert!(
                e.message.ends_with("invalid removal proof"),
                "{}",
                e.message
            );
        }
        other => panic!("not an invalid proof: {other:?}"),
    }
    unchanged(&x);

    // a holder holds no foreign copy
    let hdir = x._dir.path().join("holder");
    let mut h = Store::create_holder(&hdir, c.repo).unwrap();
    assert!(matches!(
        take(&mut h, c.repo, c.repo, c.apply, &mut c.source()),
        Err(ForeignRemovalError::Store(StoreError::Refused(_)))
    ));

    // the untampered proof is still taken: here no local change wrote v5
    let taken = take(&mut x.store, c.repo, c.repo, c.apply, &mut records).unwrap();
    assert_eq!(taken.erased, vec![pair(&c.v1), pair(&c.v2), pair(&c.v5)]);
    assert!(taken.kept_local.is_empty());
    assert!(erased(&x.path, &c.v1));
    clean(&mut x.store, 4);
}
