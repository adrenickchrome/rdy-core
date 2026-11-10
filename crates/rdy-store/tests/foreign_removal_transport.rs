//! rpl37 (repo links, D-CN4QJ): a foreign `Removal`, its source proof closure, its occurrence
//! provenance and its residues travel through native dump, load and project intake (a follower's
//! increment) as the log does. The load and the intake check the proof again over the chain's
//! own records and against its source repo, never as a local removal: its request's spans name
//! places of the cited log, so a load never reads them as places of this one, and its landing and
//! apply never become local canon authority. The sealed `Removal` is the authority for the
//! foreign occurrences it names, and for nothing else: local bytes of the same hash, and their
//! identities, survive every step. A proof or a scope that fails is refused before any byte of
//! the load or the intake is written.
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource};
use rdy_store::dump::{
    DumpBatch, DumpHeader, DumpItem, DumpReader, DumpWriter, Frame, RemovalFacts, RemovalTally,
};
use rdy_store::dump_chain::{BatchMarker, ChainEnd, DumpChainHeader};
use rdy_store::dump_scan::{ChainSource, FileRole, ScanError, ScanPlan, StopMatch, scan};
use rdy_store::follower::{self, FollowerMark, prepare_follow};
use rdy_store::foreign_removal::ForeignProof;
use rdy_store::format::DumpStore;
use rdy_store::ingest::Ingest;
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError, StoreLock, StoreReader, codec, kinds, sync_proof};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use std::collections::BTreeMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// The bytes of a log frame's header.
const HEADER: usize = 37;

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

fn change(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
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
    fn deliver(&mut self, s: &mut Store, added: &[&Block]) {
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
        self.head = Some(s.put_foreign(&e, &blocks).unwrap());
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
    _store: Store,
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
        let store = Store::open(&path).unwrap();
        Cited {
            _dir: dir,
            reader: store.reader(),
            _store: store,
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
}

/// The citing project: its own block; a live link that delivers `v1`, `v3` and `v5`; a revoked
/// link that delivered `v2`; and `v5` written locally after its delivery (one frame, the
/// delivery's, which local content now needs).
struct Citing {
    dir: tempfile::TempDir,
    path: PathBuf,
    store: Store,
    own: Block,
}

fn citing(c: &Cited) -> Citing {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("citing");
    let mut s = Store::create(&path).unwrap();
    let own = block("C-00001", "a local claim");
    s.commit_to_view("work", change(vec![Op::Create { block: own.clone() }]))
        .unwrap();
    let mut live = Source::new(c.repo, link("rl-00001"));
    live.deliver(&mut s, &[&c.v1, &c.v3, &c.v5]);
    let mut gone = Source::new(c.repo, link("rl-00002"));
    gone.deliver(&mut s, &[&c.v2]);
    gone.revoke(&mut s);
    s.commit_to_view(
        "work",
        change(vec![Op::Create {
            block: c.v5.clone(),
        }]),
    )
    .unwrap();
    Citing {
        dir,
        path,
        store: s,
        own,
    }
}

/// Takes the cited project's removal into `x`: it erases `v1` and `v2`, and keeps `v5`.
fn take_removal(c: &Cited, x: &mut Citing) {
    let taken = x
        .store
        .take_foreign_removal(
            &c.repo,
            ForeignProof {
                repo: c.repo,
                apply: c.apply,
                source: &mut c.source(),
            },
        )
        .unwrap();
    assert_eq!(taken.erased, vec![pair(&c.v1), pair(&c.v2)]);
    assert_eq!(taken.kept_local, vec![pair(&c.v5)]);
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

/// Every frame of the log at `dir`: its logical kind, hash and payload.
fn frames(dir: &Path) -> Vec<(u8, Hash, Vec<u8>)> {
    let mut out = vec![];
    for bytes in segments(dir).values() {
        let mut at = 0usize;
        while at + HEADER <= bytes.len() {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let kind = codec::logical_kind(bytes[at + 4]);
            let hash = Hash(bytes[at + 5..at + 37].try_into().unwrap());
            out.push((kind, hash, bytes[at + HEADER..at + HEADER + len].to_vec()));
            at += HEADER + len;
        }
    }
    out
}

/// The payloads of every BLOCK frame of `b`'s version in the log at `dir`.
fn block_payloads(dir: &Path, b: &Block) -> Vec<Vec<u8>> {
    frames(dir)
        .into_iter()
        .filter(|(k, h, _)| *k == kinds::BLOCK && *h == version_of(b))
        .map(|(_, _, p)| p)
        .collect()
}

fn erased(dir: &Path, b: &Block) -> bool {
    let p = block_payloads(dir, b);
    !p.is_empty() && p.iter().all(|p| p.iter().all(|x| *x == 0))
}

fn intact(dir: &Path, b: &Block) -> bool {
    let p = block_payloads(dir, b);
    !p.is_empty() && p.iter().all(|p| Hash::of(p) == version_of(b))
}

/// Whether the BLOCK of `b` reads as removed through `r`.
fn removed(r: &StoreReader, b: &Block) -> bool {
    matches!(
        r.get_kind_raw(kinds::BLOCK, &version_of(b)),
        Err(StoreError::Redacted { .. })
    )
}

/// A dump of `s`: the full dump with `prev` None, else the increment after `prev`. Returns its
/// bytes and where the chain ends after it.
fn dump(s: &Store, prev: Option<&ChainEnd>) -> (Vec<u8>, ChainEnd) {
    let r = s.reader();
    let end = s.log_end();
    let repo = r.repo_id().unwrap().unwrap();
    let end_batch = r.batch_marker_at(end).unwrap().unwrap_or(BatchMarker::ZERO);
    let (chain, base) = match prev {
        None => (DumpChainHeader::full(repo, end_batch), ORIGIN),
        Some(p) => (DumpChainHeader::after(p, end_batch), p.end),
    };
    let store = DumpStore::Features(r.store_features().unwrap());
    let header = DumpHeader {
        store: store.clone(),
        log_end: end,
        rdy: "test".into(),
        chain: Some(chain),
    };
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    let mut tally = RemovalTally::default();
    r.for_each_batch_after(base, end, &mut |b| {
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

/// The verified plan of the chain `files` that stops at the batch end `stop` (None: its end),
/// as `rdy load` reads it before any write.
fn plan_of(files: &[PathBuf], stop: Option<Location>) -> Result<ScanPlan, ScanError> {
    let mut src = ChainSource::open(files)?;
    scan(&mut src, stop.map(StopMatch::Position))
}

/// Loads the chain `files` into the new store directory `to`, as `rdy load` does: the pre-pass
/// first, then each retained batch at its source place as the plan decides, the carried batch,
/// the one upgrade and the ingest's final check.
fn load(files: &[PathBuf], to: &Path) -> Result<(), String> {
    load_to(files, None, to)
}

/// [`load`], stopped at the batch end `stop`.
fn load_to(files: &[PathBuf], stop: Option<Location>, to: &Path) -> Result<(), String> {
    let plan = plan_of(files, stop).map_err(|e| format!("scan: {e:?}"))?;
    let mut ingest = Ingest::create(to).map_err(|e| format!("create: {e}"))?;
    let mut n = 0;
    for (i, path) in files.iter().enumerate() {
        let facts = &plan.files()[i];
        if facts.role == FileRole::After {
            continue;
        }
        let f = BufReader::new(std::fs::File::open(path).unwrap());
        let (mut reader, _, _) = DumpReader::open(f).unwrap();
        let mut kept = 0;
        while let Some(item) = reader.next().unwrap() {
            if let DumpItem::Batch(b) = item
                && kept < facts.kept_batches
            {
                kept += 1;
                n += 1;
                ingest
                    .batch_with_plan(n, &b, &plan)
                    .map_err(|e| format!("batch: {e}"))?;
            }
        }
    }
    ingest
        .append_carried(plan.residues(), plan.carried())
        .map_err(|e| format!("carried: {e}"))?;
    ingest
        .upgrade(plan.store())
        .map_err(|e| format!("upgrade: {e}"))?;
    ingest.finish().map_err(|e| format!("finish: {e}"))?;
    Ok(())
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

/// Loads the full dump `bytes` into `to` and marks it a follower at `chain`.
fn seed_follower(file: &Path, chain: &ChainEnd, to: &Path) {
    load(&[file.to_path_buf()], to).unwrap();
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

/// Takes the increment at `inc` into the follower at `dir`, as a project replica's intake does.
fn intake(dir: &Path, inc: &Path) -> Result<(), String> {
    let mut f = Store::open_follower(dir).map_err(|e| format!("open: {e}"))?;
    let mark = follower::read_mark(dir).unwrap().unwrap();
    let v = prepare_follow(&f, &mark, inc).map_err(|e| format!("prepare: {e:?}"))?;
    let next = v.chain().clone();
    f.follow_verified(v).map_err(|e| format!("follow: {e:?}"))?;
    f.write_follow_mark(&FollowerMark {
        chain: next,
        files: vec![],
    })
    .unwrap();
    f.close().unwrap();
    Ok(())
}

/// Removes the derived index of the store at `dir`: the next open rebuilds it from the log.
fn drop_index(dir: &Path) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        if e.file_name().to_string_lossy().starts_with("index") {
            let p = e.path();
            if p.is_dir() {
                std::fs::remove_dir_all(p).unwrap();
            } else {
                std::fs::remove_file(p).unwrap();
            }
        }
    }
}

/// What every replica must show after the removal: `v1` and `v2` are zeros on disk and read as
/// removed; `v3`, `v5` and the own block are intact, readable and keep their identities; the
/// proof verifies over the replica's own records and still approves exactly its four
/// versions; fsck passes with 4 foreign entries (two deliveries, a revocation, the removal).
fn holds_the_removal(c: &Cited, own: &Block, dir: &Path, r: &StoreReader, what: &str) {
    for b in [&c.v1, &c.v2] {
        assert!(erased(dir, b), "{what}: {} erased", b.id);
        assert!(removed(r, b), "{what}: {} reads as removed", b.id);
    }
    for b in [&c.v3, &c.v5, own] {
        assert!(intact(dir, b), "{what}: {} intact", b.id);
        assert_eq!(r.get_block(&version_of(b)).unwrap(), *b, "{what}");
    }
    let removal = sync_proof::check(
        &mut StoreSource {
            reader: r,
            end: END_OF_LOG,
        },
        c.apply,
    )
    .unwrap_or_else(|e| panic!("{what}: the proof verifies: {e}"));
    let approved: Vec<Hash> = removal
        .objects
        .keys()
        .filter(|k| k.kind == kinds::BLOCK)
        .map(|k| k.hash)
        .collect();
    let mut want: Vec<Hash> = [&c.v1, &c.v2, &c.v4, &c.v5]
        .iter()
        .map(|b| version_of(b))
        .collect();
    want.sort();
    assert_eq!(approved, want, "{what}");
    assert_eq!(r.check_objects().unwrap().foreign, 4, "{what}");
}

#[test]
fn rpl37_removal_roundtrip_preserves_authority_and_matching_local_bytes() {
    let c = Cited::new();
    let mut x = citing(&c);
    let work = x.dir.path().to_path_buf();
    let local_head = x.store.reader().view_head("work").unwrap();
    let canon = x.store.canon_head().unwrap();

    // a replica seeded before the removal holds every foreign copy intact
    let (pre, pre_end) = dump(&x.store, None);
    let pre_file = write(&work, "pre.rdydump", &pre);
    let fdir = work.join("replica");
    seed_follower(&pre_file, &pre_end, &fdir);
    assert!(intact(&fdir, &c.v1) && intact(&fdir, &c.v2) && intact(&fdir, &c.v5));

    take_removal(&c, &mut x);
    holds_the_removal(&c, &x.own, &x.path, &x.store.reader(), "source");

    // native dump and load: the log byte for byte, its holes as zeros
    let (full, _) = dump(&x.store, None);
    let full_file = write(&work, "full.rdydump", &full);
    let ldir = work.join("loaded");
    load(std::slice::from_ref(&full_file), &ldir).unwrap();
    assert_eq!(
        segments(&ldir),
        segments(&x.path),
        "the load writes the log"
    );
    {
        let mut l = Store::open(&ldir).unwrap();
        holds_the_removal(&c, &x.own, &ldir, &l.reader(), "loaded");
        assert_eq!(l.reader().view_head("work").unwrap(), local_head);
        assert_eq!(l.canon_head().unwrap(), canon);
        l.reindex().unwrap();
        holds_the_removal(&c, &x.own, &ldir, &l.reader(), "loaded, reindexed");
        // a dump of the loaded store is the source's dump
        assert_eq!(dump(&l, None).0, full, "a re-dump");
        l.close().unwrap();
    }
    drop_index(&ldir);
    let l = Store::open(&ldir).unwrap();
    holds_the_removal(&c, &x.own, &ldir, &l.reader(), "loaded, index rebuilt");
    drop(l);

    // the chain of the earlier full dump and the increment that holds the removal
    let (inc, _) = dump(&x.store, Some(&pre_end));
    let inc_file = write(&work, "inc.rdydump", &inc);
    let mdir = work.join("chain");
    load(&[pre_file.clone(), inc_file.clone()], &mdir).unwrap();
    assert_eq!(
        segments(&mdir),
        segments(&x.path),
        "the chain load writes the log"
    );
    {
        let mut m = Store::open(&mdir).unwrap();
        holds_the_removal(&c, &x.own, &mdir, &m.reader(), "chain");
        m.reindex().unwrap();
        holds_the_removal(&c, &x.own, &mdir, &m.reader(), "chain, reindexed");
    }

    // project intake: the replica takes the increment; its open erases the same occurrences
    intake(&fdir, &inc_file).unwrap();
    assert_eq!(
        segments(&fdir),
        segments(&x.path),
        "the intake writes the log"
    );
    let f = Store::open_follower(&fdir).unwrap();
    holds_the_removal(&c, &x.own, &fdir, &f.reader(), "replica");
    drop(f);
    drop_index(&fdir);
    let f = Store::open_follower(&fdir).unwrap();
    holds_the_removal(&c, &x.own, &fdir, &f.reader(), "replica, index rebuilt");
    drop(f);

    // the source, reindexed, still agrees
    x.store.reindex().unwrap();
    holds_the_removal(&c, &x.own, &x.path, &x.store.reader(), "source, reindexed");
}

/// The text of a load's refusal in its pre-pass.
fn refusal(e: ScanError) -> String {
    match e {
        ScanError::Input {
            source: StoreError::Dump { what, .. },
            ..
        } => what,
        other => panic!("not a dump refusal: {other:?}"),
    }
}

#[test]
fn rpl37_foreign_proof_never_enters_local_apply_validation() {
    let c = Cited::new();
    let mut x = citing(&c);
    let secret = block("C-00002", "a local secret");
    x.store
        .commit_to_view(
            "work",
            change(vec![Op::Create {
                block: secret.clone(),
            }]),
        )
        .unwrap();
    let work = x.dir.path().to_path_buf();
    let (pre, pre_end) = dump(&x.store, None);
    let pre_file = write(&work, "pre.rdydump", &pre);
    take_removal(&c, &mut x);

    // a person removes the local secret after the foreign removal: a local removal beside it
    x.store.close().unwrap();
    let native = authorized_batch_fixture(
        &x.path,
        &[RequestSpec::new(
            "work",
            vec![RedactTarget::Version {
                id: secret.id.clone(),
                version: version_of(&secret),
            }],
        )
        // another time than the cited request's, so another carrier
        .at(70)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let s = Store::open(&x.path).unwrap();
    let native_apply = native.applies[0].0;
    let native_request = native.requests[0].request;
    let native_carrier = native.requests[0].carrier;
    let foreign_request = c
        .reader
        .redaction_of(kinds::BLOCK, &version_of(&c.v1))
        .unwrap()
        .expect("the cited project removed v1");
    let foreign_carrier = c
        .reader
        .redaction_request(&foreign_request)
        .unwrap()
        .carrier;
    assert!(removed(&s.reader(), &secret));
    let canon = s.canon_head().unwrap();

    // the full dump: each removal is checked by its own rule, and no carried record is made
    let (full, _) = dump(&s, None);
    let full_file = write(&work, "full.rdydump", &full);
    let plan = plan_of(std::slice::from_ref(&full_file), None).unwrap();
    assert!(plan.carried().is_empty());
    let ldir = work.join("loaded");
    load(std::slice::from_ref(&full_file), &ldir).unwrap();
    assert_eq!(segments(&ldir), segments(&x.path));
    let l = Store::open(&ldir).unwrap();
    let r = l.reader();
    holds_the_removal(&c, &x.own, &ldir, &r, "loaded");
    assert!(erased(&ldir, &secret) && removed(&r, &secret));
    // the local removal is the local request's; the foreign one removes the foreign versions
    // only; the local copy of v5, which the proof also approves, is no one's
    assert_eq!(
        r.redaction_of(kinds::BLOCK, &version_of(&secret)).unwrap(),
        Some(native_request)
    );
    for b in [&c.v1, &c.v2] {
        assert_eq!(
            r.redaction_of(kinds::BLOCK, &version_of(b)).unwrap(),
            Some(foreign_request)
        );
    }
    assert_eq!(
        r.redaction_of(kinds::BLOCK, &version_of(&c.v5)).unwrap(),
        None
    );
    // the proof's landing never becomes local canon authority; the local one does
    assert_eq!(r.landing_of(&foreign_carrier).unwrap(), None);
    assert_eq!(r.landing_of(&native_carrier).unwrap(), native.landing);
    assert_eq!(l.canon_head().unwrap(), canon);
    drop(r);
    drop(l);

    // a load stopped before both removals: the local one is carried; the foreign proof gives no
    // carried record (it stays in the foreign log), so the earlier file's foreign copies load as
    // that file holds them
    let (inc, _) = dump(&s, Some(&pre_end));
    let inc_file = write(&work, "inc.rdydump", &inc);
    let chain = [pre_file.clone(), inc_file.clone()];
    let stopped = plan_of(&chain, Some(pre_end.end)).unwrap();
    let applies: Vec<Hash> = stopped.carried().iter().map(|r| r.apply).collect();
    assert_eq!(applies, vec![native_apply]);
    let sdir = work.join("stopped");
    load_to(&chain, Some(pre_end.end), &sdir).unwrap();
    let st = Store::open(&sdir).unwrap();
    for b in [&c.v1, &c.v2, &c.v5] {
        assert!(intact(&sdir, b), "{}", b.id);
        assert_eq!(
            st.reader()
                .redaction_of(kinds::BLOCK, &version_of(b))
                .unwrap(),
            None
        );
    }
    // two deliveries and a revocation: the removal lies after the stop
    assert_eq!(st.reader().check_objects().unwrap().foreign, 3);
    drop(st);

    // the full dump stopped before them: a foreign copy that only the later foreign removal
    // dropped has no authority in the retained prefix, and nothing is written
    let e = plan_of(std::slice::from_ref(&full_file), Some(pre_end.end)).unwrap_err();
    assert_eq!(refusal(e), "dropped frame has no valid authorization");
    let gone = work.join("stopped-full");
    assert!(load_to(std::slice::from_ref(&full_file), Some(pre_end.end), &gone).is_err());
    assert!(!gone.exists());
    drop(s);
}

/// The dump `bytes` with its last batch's frames changed by `f`, its batch digest, end marker
/// and envelope made again (its trailer's removal facts kept): a dump whose framing passes and
/// whose removal does not.
fn tamper(bytes: &[u8], f: &dyn Fn(&mut Vec<Frame>)) -> Vec<u8> {
    let (mut reader, _, mut header) = DumpReader::open(bytes).unwrap();
    let mut batches: Vec<DumpBatch> = vec![];
    while let Some(item) = reader.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            batches.push(b);
        }
    }
    let t = reader.trailer().unwrap().clone();
    let last = batches.last_mut().unwrap();
    let len = |b: &DumpBatch| -> u32 { b.frames.iter().map(|f| f.original_len()).sum() };
    let before = len(last);
    f(&mut last.frames);
    assert_eq!(len(last), before, "a tamper keeps every place");
    last.digest = kinds::batch_digest(last.frames.iter().map(|f| f.hash()));
    header.chain.as_mut().unwrap().end_batch = BatchMarker::of(last);
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    for b in &batches {
        w.batch(b).unwrap();
    }
    w.finish(RemovalFacts {
        removals: t.removals,
        first_removed: t.first_removed,
    })
    .unwrap()
    .0
}

/// Changes the `Removal` entry among `frames` by `f`, in place (its length kept).
fn edit_entry(frames: &mut [Frame], f: impl Fn(&mut ForeignEntry)) {
    for fr in frames.iter_mut() {
        let Frame::Present { kind, payload, .. } = fr else {
            continue;
        };
        if codec::logical_kind(*kind) != kinds::FOREIGN {
            continue;
        }
        let mut e: ForeignEntry = codec::decode_record(*kind, payload).unwrap();
        if !matches!(e.event, ForeignEvent::Removal { .. }) {
            continue;
        }
        f(&mut e);
        let (raw, bytes) = codec::item_new(kinds::FOREIGN, &e);
        *fr = Frame::Present {
            kind: raw,
            hash: Hash::of(&bytes),
            payload: bytes,
        };
        return;
    }
    panic!("no Removal entry in the batch");
}

/// Replaces the versions of the `Removal` entry among `frames` with `versions`.
fn entry_versions(frames: &mut [Frame], versions: Vec<(BlockId, Hash)>) {
    edit_entry(frames, |e| {
        let ForeignEvent::Removal { versions: v, .. } = &mut e.event else {
            unreachable!("a Removal entry")
        };
        *v = versions.clone();
    });
}

#[test]
fn rpl37_invalid_removal_transport_writes_nothing() {
    let c = Cited::new();
    let mut x = citing(&c);
    let work = x.dir.path().to_path_buf();
    let (pre, pre_end) = dump(&x.store, None);
    let pre_file = write(&work, "pre.rdydump", &pre);
    let fdir = work.join("replica");
    seed_follower(&pre_file, &pre_end, &fdir);
    take_removal(&c, &mut x);
    let (full, _) = dump(&x.store, None);
    let (inc, _) = dump(&x.store, Some(&pre_end));

    let (v1, v2, v3, v5) = (pair(&c.v1), pair(&c.v2), pair(&c.v3), pair(&c.v5));
    let cases: Vec<(&str, Box<dyn Fn(&mut Vec<Frame>)>)> = vec![
        // the entry claims another repo, whose deliveries hold none of its versions
        (
            "another repo",
            Box::new(|fs: &mut Vec<Frame>| edit_entry(fs, |e| e.repo = RepoId([9; 16]))),
        ),
        // the entry names a delivered version that the proof does not approve
        (
            "an unapproved version",
            Box::new(move |fs: &mut Vec<Frame>| entry_versions(fs, vec![v2.clone(), v3.clone()])),
        ),
        // the entry names a version that local content holds
        (
            "local content",
            Box::new(move |fs: &mut Vec<Frame>| entry_versions(fs, vec![v1.clone(), v5.clone()])),
        ),
        // the apply names a landing that no record holds: the proof fails
        (
            "a proof that fails",
            Box::new(|fs: &mut Vec<Frame>| {
                let mut apply = None;
                for fr in fs.iter_mut() {
                    if let Frame::Present { kind, payload, .. } = fr
                        && *kind == kinds::REDACTION_APPLY
                    {
                        let mut a: rdy_model::redaction::RedactionApply =
                            codec::decode_record(*kind, payload).unwrap();
                        a.landing = Hash([7; 32]);
                        let (raw, bytes) = codec::item(kinds::REDACTION_APPLY, &a).unwrap();
                        let h = Hash::of(&bytes);
                        *fr = Frame::Present {
                            kind: raw,
                            hash: h,
                            payload: bytes,
                        };
                        apply = Some(h);
                    }
                }
                let apply = apply.expect("an apply in the batch");
                edit_entry(fs, |e| {
                    let ForeignEvent::Removal { proof, .. } = &mut e.event else {
                        unreachable!("a Removal entry")
                    };
                    *proof = apply;
                });
            }),
        ),
    ];
    let replica = segments(&fdir);
    for (i, (what, f)) in cases.iter().enumerate() {
        // a load: refused by its pre-pass, before the target exists
        let bad = write(
            &work,
            &format!("full-{i}.rdydump"),
            &tamper(&full, f.as_ref()),
        );
        let e = plan_of(std::slice::from_ref(&bad), None).unwrap_err();
        assert_eq!(
            refusal(e),
            "dropped frame has no valid authorization",
            "{what}: the load"
        );
        let to = work.join(format!("load-{i}"));
        assert!(load(std::slice::from_ref(&bad), &to).is_err(), "{what}");
        assert!(!to.exists(), "{what}: nothing written");

        // a chain load whose increment holds the removal
        let bad_inc = write(
            &work,
            &format!("inc-{i}.rdydump"),
            &tamper(&inc, f.as_ref()),
        );
        let e = plan_of(&[pre_file.clone(), bad_inc.clone()], None).unwrap_err();
        assert_eq!(
            refusal(e),
            "dropped frame has no valid authorization",
            "{what}: the chain load"
        );
        let to = work.join(format!("chain-{i}"));
        assert!(load(&[pre_file.clone(), bad_inc.clone()], &to).is_err());
        assert!(!to.exists(), "{what}: nothing written");

        // the replica's intake: refused before any append
        let e = intake(&fdir, &bad_inc).unwrap_err();
        assert!(e.starts_with("prepare:"), "{what}: {e}");
        assert_eq!(segments(&fdir), replica, "{what}: the replica is unchanged");
    }

    // the honest increment is still taken
    let inc_file = write(&work, "inc.rdydump", &inc);
    intake(&fdir, &inc_file).unwrap();
    assert!(erased(&fdir, &c.v1) && erased(&fdir, &c.v2));
    assert!(intact(&fdir, &c.v5) && intact(&fdir, &c.v3));
}
