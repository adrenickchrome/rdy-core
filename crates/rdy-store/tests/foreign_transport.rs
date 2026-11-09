//! rpl11 (repo links, D-FK58A): the foreign log travels through native dump and load and through
//! project replication (the follower) as the log does: byte for byte, with its held versions,
//! its feature manifest and its source refs. A load or a follow keeps it outside canon, never
//! resets a pin and never drops an entry. An interrupted take or follow keeps every acknowledged
//! batch and nothing of the torn one. The foreign bytes count every held copy of a repo, a
//! revoked source's included, and the log byte report counts every FOREIGN frame.
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignId, ForeignParts, ForeignPin,
    ForeignPublication, ForeignShell, ForeignSource, ForeignVersion, delivery_selection,
};
use rdy_model::*;
use rdy_store::dump::{
    DumpBatch, DumpHeader, DumpItem, DumpReader, DumpWriter, Frame, RemovalTally,
};
use rdy_store::dump_chain::{BatchMarker, ChainEnd, DumpChainHeader};
use rdy_store::follower::{self, FollowStep, FollowerMark, on_follow_step, prepare_follow};
use rdy_store::format::{self, DumpStore, FEATURE_FOREIGN_COPY, StoreFeatures};
use rdy_store::ingest::Ingest;
use rdy_store::segment::Location;
use rdy_store::{Store, StoreLock, StoreReader, codec, kinds};
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

fn cited_repo() -> RepoId {
    RepoId([7; 16])
}

fn other_repo() -> RepoId {
    RepoId([9; 16])
}

/// A cited block, as the cited side delivers it.
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

/// The BLOCK version of `b`: the hash of its frame.
fn version_of(b: &Block) -> Hash {
    Hash::of(&codec::item_new(kinds::BLOCK, b).1)
}

fn fv(b: &Block) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: version_of(b),
        content: b.content_hash(),
        encoding: 0,
        derived: ForeignDerived {
            status: Some("accepted".into()),
            lifecycle: None,
            superseded_by: vec![],
        },
    }
}

fn link(link: &str) -> ForeignSource {
    ForeignSource::Link {
        link: link.into(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    }
}

fn adr(name: &str) -> ForeignSource {
    ForeignSource::Adr {
        source: name.into(),
        revision: "r1".into(),
    }
}

/// The citing side's own record of one source, to build each delivery's prev and digest.
struct Source {
    repo: RepoId,
    source: ForeignSource,
    head: Option<Hash>,
    sel: BTreeMap<BlockId, Hash>,
    /// The shells the next delivery names (D-2CG1N): the complete list, not a delta.
    shells: Vec<ForeignShell>,
}

impl Source {
    fn new(repo: RepoId, source: ForeignSource) -> Source {
        Source {
            repo,
            source,
            head: None,
            sel: BTreeMap::new(),
            shells: vec![],
        }
    }

    /// Takes a delivery of `added`, `changed` and `removed` into `s`, with the blocks of
    /// `added` and `changed`.
    fn deliver(&mut self, s: &mut Store, added: &[&Block], changed: &[&Block], removed: &[&str]) {
        let entry = self.delivery(added, changed, removed);
        let blocks: Vec<Block> = added.iter().chain(changed).map(|b| (*b).clone()).collect();
        self.head = Some(s.put_foreign(&entry, &blocks).unwrap());
    }

    /// The next delivery's entry, and this source's record moved past it.
    fn delivery(&mut self, added: &[&Block], changed: &[&Block], removed: &[&str]) -> ForeignEntry {
        for b in added.iter().chain(changed) {
            self.sel.insert(b.id.clone(), version_of(b));
        }
        for r in removed {
            self.sel.remove(&id(r));
        }
        ForeignEntry {
            repo: self.repo,
            source: self.source.clone(),
            event: ForeignEvent::Delivery {
                prev: self.head,
                publication: ForeignPublication {
                    tag: "v1".into(),
                    profile: "decisions".into(),
                    profile_version: Some(Hash([3; 32])),
                },
                set: None,
                added: added.iter().map(|b| fv(b)).collect(),
                changed: changed.iter().map(|b| fv(b)).collect(),
                removed: removed.iter().map(|r| id(r)).collect(),
                shells: self.shells.clone(),
                selection: delivery_selection(self.sel.iter(), &self.shells).unwrap(),
            },
        }
    }

    /// Revokes this source in `s`.
    fn revoke(&mut self, s: &mut Store) {
        let entry = ForeignEntry {
            repo: self.repo,
            source: self.source.clone(),
            event: ForeignEvent::Revoked {
                prev: self.head.unwrap(),
            },
        };
        self.head = Some(s.put_foreign(&entry, &[]).unwrap());
    }
}

/// A local block that pins `target` at its delivered content.
fn pinning(local: &str, target: &Block) -> Block {
    let mut b = block(local, "cites the hub");
    b.block_type = "claim".into();
    b.foreign = ForeignParts {
        links: BTreeMap::new(),
        pins: [(
            ForeignId::new(cited_repo(), target.id.clone()),
            ForeignPin {
                content: target.content_hash(),
                encoding: 0,
            },
        )]
        .into(),
    };
    b
}

fn change(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: Provenance {
            actor: "agent:a".into(),
            run: None,
            role: Role::Writer,
            at_ms: 10,
        },
        justification: None,
        message: "local".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// Everything a reader of the foreign log sees: each source ref and its view, the current view
/// of the cited repo, and the foreign bytes of each repo.
fn foreign_state(r: &StoreReader) -> String {
    let mut out = String::new();
    for (name, head) in r.foreign_sources().unwrap() {
        out.push_str(&format!(
            "{name} {head} {:?}\n",
            r.source_view(&name).unwrap()
        ));
    }
    out.push_str(&format!("{:?}\n", r.repo_view(&cited_repo()).unwrap()));
    for repo in [cited_repo(), other_repo()] {
        out.push_str(&format!(
            "{repo} {} bytes\n",
            r.foreign_bytes(&repo).unwrap()
        ));
    }
    out
}

/// Every ref of the store, by name.
fn refs(r: &StoreReader) -> Vec<(String, Vec<u8>)> {
    r.refs_with_prefix("").unwrap()
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

/// The log of the store at `dir` up to `end`: each segment's bytes before `end`.
fn log_prefix(dir: &Path, end: Location) -> Vec<Vec<u8>> {
    let all = segments(dir);
    (0..=end.segment)
        .map(|seg| {
            let bytes = all
                .get(&format!("seg-{seg:06}.log"))
                .cloned()
                .unwrap_or_default();
            if seg == end.segment {
                bytes[..end.offset as usize].to_vec()
            } else {
                bytes
            }
        })
        .collect()
}

/// Copies the durable record of the store at `from` (its `FORMAT` and `FEATURES` mirrors, a
/// follower mark and its segments) to `to`, which must not exist, as a kill would leave it on
/// disk. The lock and the derived index stay behind: the copy opens by its log.
fn copy_record(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.join("segments")).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        if e.file_type().unwrap().is_file() && !name.starts_with("index") && name != "lock" {
            std::fs::copy(e.path(), to.join(&name)).unwrap();
        }
    }
    for e in std::fs::read_dir(from.join("segments")).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), to.join("segments").join(e.file_name())).unwrap();
    }
}

/// A dump of the store `s` read by `r`: the full dump with `prev` None, else the increment after
/// `prev`. Returns its bytes and where the chain ends after it.
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
    let next = ChainEnd {
        repo_id: repo,
        store_format: store,
        end,
        end_batch,
        digest: totals.digest,
    };
    (bytes, next)
}

/// Loads the full dump `bytes` into the new store directory `to`, as `rdy load` does: each batch
/// at its source place, the dump's store kept, the drops checked at the end.
fn load(bytes: &[u8], to: &Path) {
    let (mut reader, _, header) = DumpReader::open(bytes).unwrap();
    let mut ingest = Ingest::create(to).unwrap();
    let mut n = 0;
    while let Some(item) = reader.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            n += 1;
            ingest.batch(n, &b).unwrap();
        }
    }
    assert_eq!(ingest.end(), header.log_end);
    ingest.upgrade(&header.store).unwrap();
    ingest.finish().unwrap();
}

/// The cited side's blocks: D-00001 in two versions, D-00002, D-00003 and D-00004.
struct Cited {
    a1: Block,
    a2: Block,
    b1: Block,
    c1: Block,
    d1: Block,
}

fn cited() -> Cited {
    Cited {
        a1: block("D-00001", "first"),
        a2: block("D-00001", "first, revised"),
        b1: block("D-00002", "second"),
        c1: block("D-00003", "third"),
        d1: block("D-00004", "fourth"),
    }
}

/// A project store with local content, then a foreign log of every source shape: a live link
/// that adds, changes and removes; a revoked link; an ADR source; and a link of another repo.
/// A local block pins a replaced version, through a change and as a bare block.
fn history(dir: &Path, c: &Cited) -> Store {
    let mut s = Store::create(dir).unwrap();
    s.commit_to_view(
        "work",
        change(vec![Op::Create {
            block: block("C-00001", "local claim"),
        }]),
    )
    .unwrap();
    let mut live = Source::new(cited_repo(), link("rl-00001"));
    live.deliver(&mut s, &[&c.a1, &c.b1], &[], &[]);
    // a local citation of the first delivery's D-00001
    s.commit_to_view(
        "work",
        change(vec![Op::Create {
            block: pinning("C-00002", &c.a1),
        }]),
    )
    .unwrap();
    // the second delivery keeps a planned doc as a shell: the entry carries it, no block travels
    live.shells = vec![ForeignShell {
        id: id("D-00010"),
        version: Hash([5; 32]),
    }];
    live.deliver(&mut s, &[&c.c1], &[&c.a2], &["D-00002"]);
    s.put(kinds::BLOCK, &pinning("C-00003", &c.a2)).unwrap();
    let mut gone = Source::new(cited_repo(), link("rl-00002"));
    gone.deliver(&mut s, &[&c.d1, &c.b1], &[], &[]);
    gone.revoke(&mut s);
    let mut adrs = Source::new(cited_repo(), adr("adrs"));
    adrs.deliver(&mut s, &[&c.b1], &[], &[]);
    let mut other = Source::new(other_repo(), link("rl-00003"));
    other.deliver(&mut s, &[&block("D-00009", "of another repo")], &[], &[]);
    s
}

#[test]
fn rpl11_dump_load_preserves_foreign_history() {
    let d = tempfile::tempdir().unwrap();
    let c = cited();
    let src = d.path().join("src");
    let s = history(&src, &c);
    assert!(s.features().has(FEATURE_FOREIGN_COPY));
    let want = foreign_state(&s.reader());
    let want_refs = refs(&s.reader());
    let (bytes, chain) = dump(&s, None);
    let DumpStore::Features(m) = &chain.store_format else {
        panic!("a feature dump");
    };
    assert!(m.has(FEATURE_FOREIGN_COPY), "the dump names foreign_copy");

    let to = d.path().join("to");
    load(&bytes, &to);
    // the log is the source's, byte for byte, and FORMAT mirrors its manifest
    assert_eq!(segments(&to), segments(&src));
    assert_eq!(
        format::read_format_file(&to).unwrap().as_ref(),
        Some(s.features())
    );
    let mut t = Store::open(&to).unwrap();
    assert_eq!(t.log_end(), s.log_end(), "the load appends nothing");
    assert!(t.features().has(FEATURE_FOREIGN_COPY));
    // every source, held version, revocation, ref and pin is the source's; no ref moved
    assert_eq!(foreign_state(&t.reader()), want);
    assert_eq!(refs(&t.reader()), want_refs);
    let r = t.reader();
    for (b, held) in [(&c.a1, true), (&c.a2, true), (&c.b1, true), (&c.d1, true)] {
        let got = r
            .held_version(&ForeignId::new(cited_repo(), b.id.clone()), &version_of(b))
            .unwrap();
        assert_eq!(got.is_some(), held, "{}", b.body);
    }
    let gone = r.source_view("foreign/link/rl-00002").unwrap().unwrap();
    assert!(gone.revoked, "the revocation travels");
    // the delivery's shells travel in its entry, and its selection still rebuilds with them
    let live = r.source_view("foreign/link/rl-00001").unwrap().unwrap();
    let ForeignEvent::Delivery { shells, .. } = r.foreign_entry(&live.head).unwrap().event else {
        panic!("the live source's head is a delivery");
    };
    assert_eq!(
        shells,
        vec![ForeignShell {
            id: id("D-00010"),
            version: Hash([5; 32]),
        }]
    );
    assert_eq!(
        r.check_objects().unwrap().foreign,
        6,
        "every foreign entry passes fsck"
    );
    // the pins are the local blocks', unchanged
    let pinned = r
        .get_block(&version_of(&pinning("C-00003", &c.a2)))
        .unwrap();
    assert_eq!(pinned.foreign, pinning("C-00003", &c.a2).foreign);
    // a rebuilt index finds the same refs and features from the log alone
    t.reindex().unwrap();
    assert_eq!(foreign_state(&t.reader()), want);
    assert_eq!(refs(&t.reader()), want_refs);
    t.close().unwrap();
    // an index lost with its features ref is rebuilt from the log at open
    std::fs::remove_file(to.join("index.redb")).unwrap();
    let t = Store::open(&to).unwrap();
    assert!(t.features().has(FEATURE_FOREIGN_COPY));
    assert_eq!(foreign_state(&t.reader()), want);
    // a dump of the loaded store is the same file
    assert_eq!(dump(&t, None).0, bytes);

    // a holder's closed refs never receive a foreign entry: a holder dump that carries one (its
    // manifest names foreign_copy, so the dump reads) is refused before the batch is written
    let hd = d.path().join("holder");
    let mut h = Store::create_holder(&hd, RepoId([1; 16])).unwrap();
    let held_end = h.log_end();
    let entry = Source::new(cited_repo(), link("rl-00001")).delivery(&[], &[], &[]);
    let e = h.put(kinds::FOREIGN, &entry).unwrap();
    assert!(h.features().has(FEATURE_FOREIGN_COPY));
    let (bytes, _) = dump(&h, None);
    let to = d.path().join("holder-to");
    let (mut reader, _, _) = DumpReader::open(&bytes[..]).unwrap();
    let mut ingest = Ingest::create(&to).unwrap();
    let mut n = 0;
    let refused = loop {
        let Some(item) = reader.next().unwrap() else {
            panic!("the holder dump loaded");
        };
        if let DumpItem::Batch(b) = item {
            n += 1;
            if let Err(e) = ingest.batch(n, &b) {
                break e;
            }
        }
    };
    match refused {
        rdy_store::StoreError::Decode(m) => {
            assert_eq!(m, format!("holder holds foreign entry {e}"))
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(ingest.end(), held_end, "the refused batch writes nothing");

    // a foreign frame is never dropped: one of a dump whose manifest does not name foreign_copy,
    // or of an encoding this rdy does not know, is refused
    let (raw, payload) = codec::item_new(kinds::FOREIGN, &entry);
    let batch = |payload: Vec<u8>| DumpBatch {
        start: ORIGIN,
        digest: Hash([0; 32]),
        frames: vec![Frame::Present {
            kind: raw,
            hash: Hash::of(&payload),
            payload,
        }],
    };
    let header = |store: StoreFeatures| DumpHeader {
        store: DumpStore::Features(store),
        log_end: ORIGIN,
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(RepoId([1; 16]), BatchMarker::ZERO)),
    };
    let refusal = |store: StoreFeatures, payload: Vec<u8>| {
        let mut w = DumpWriter::new(Vec::new(), &header(store)).unwrap();
        match w.batch(&batch(payload)) {
            Err(rdy_store::StoreError::Refused(m)) => m,
            other => panic!("{other:?}"),
        }
    };
    assert!(!format::baseline_features().has(FEATURE_FOREIGN_COPY));
    assert_eq!(
        refusal(format::baseline_features(), payload.clone()),
        "a record of kind 157 cannot stand here"
    );
    let mut later = payload;
    later[..2].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        refusal(s.features().clone(), later),
        "unknown encoding version"
    );
}

/// A follower seeded from a full dump of `src`'s store: its directory, its saved chain end, and
/// the temporary directory that holds the files.
struct Follower {
    dir: PathBuf,
    chain: ChainEnd,
}

fn seed(s: &Store, dir: &Path) -> Follower {
    let (bytes, chain) = dump(s, None);
    load(&bytes, dir);
    let lock = StoreLock::take(dir).unwrap();
    follower::write_seed_mark(
        dir,
        &lock,
        &FollowerMark {
            chain: chain.clone(),
            files: vec![],
        },
    )
    .unwrap();
    drop(lock);
    Follower {
        dir: dir.to_path_buf(),
        chain,
    }
}

/// Follows the increment at `inc` into the follower at `dir` and saves its new mark. Returns the
/// chain end it saved.
fn follow(dir: &Path, inc: &Path) -> ChainEnd {
    let mut f = Store::open_follower(dir).unwrap();
    let mark = follower::read_mark(dir).unwrap().unwrap();
    let v = prepare_follow(&f, &mark, inc).unwrap();
    let next = v.chain().clone();
    f.follow_verified(v).unwrap();
    f.write_follow_mark(&FollowerMark {
        chain: next.clone(),
        files: vec![],
    })
    .unwrap();
    f.close().unwrap();
    next
}

#[test]
fn rpl11_follower_replays_foreign_only_batch() {
    let d = tempfile::tempdir().unwrap();
    let c = cited();
    let src = d.path().join("src");
    let mut s = Store::create(&src).unwrap();
    s.commit_to_view(
        "work",
        change(vec![Op::Create {
            block: block("C-00001", "local claim"),
        }]),
    )
    .unwrap();
    assert!(!s.features().has(FEATURE_FOREIGN_COPY));
    let f = seed(&s, &d.path().join("follower"));
    let local_refs = refs(&s.reader());

    // foreign-only batches: the first use of foreign_copy, a delta and a revocation
    let mut live = Source::new(cited_repo(), link("rl-00001"));
    live.deliver(&mut s, &[&c.a1, &c.b1], &[], &[]);
    live.deliver(&mut s, &[], &[&c.a2], &["D-00002"]);
    let mut gone = Source::new(cited_repo(), link("rl-00002"));
    gone.deliver(&mut s, &[&c.d1], &[], &[]);
    gone.revoke(&mut s);
    let (inc, _) = dump(&s, Some(&f.chain));
    let inc_path = d.path().join("inc.rdydump");
    std::fs::write(&inc_path, &inc).unwrap();

    let next = follow(&f.dir, &inc_path);
    assert_eq!(next.end, s.log_end());
    let DumpStore::Features(m) = &next.store_format else {
        panic!("a feature chain");
    };
    assert!(
        m.has(FEATURE_FOREIGN_COPY),
        "the mark names the new manifest"
    );
    // the follower's log is the source's; its FORMAT and features follow the first use
    assert_eq!(segments(&f.dir), segments(&src));
    assert_eq!(
        format::read_format_file(&f.dir).unwrap().as_ref(),
        Some(s.features())
    );
    let t = Store::open_follower(&f.dir).unwrap();
    assert!(t.features().has(FEATURE_FOREIGN_COPY));
    assert_eq!(foreign_state(&t.reader()), foreign_state(&s.reader()));
    assert_eq!(refs(&t.reader()), refs(&s.reader()));
    // only foreign refs moved: canon and every local ref are as the seed left them
    let moved: Vec<String> = refs(&t.reader())
        .into_iter()
        .filter(|r| !local_refs.contains(r))
        .map(|(name, _)| name)
        .collect();
    assert!(
        moved
            .iter()
            .all(|n| n.starts_with("foreign/") || n == "meta/store_features"),
        "{moved:?}"
    );
    assert_eq!(t.reader().check_objects().unwrap().foreign, 4);
}

#[test]
fn rpl11_interrupted_take_keeps_acknowledged_log() {
    let d = tempfile::tempdir().unwrap();
    let c = cited();

    // a take killed before its seal: the first foreign use, in a held batch its writer seals
    let src = d.path().join("src");
    let mut s = Store::create(&src).unwrap();
    s.commit_to_view(
        "work",
        change(vec![Op::Create {
            block: block("C-00001", "local claim"),
        }]),
    )
    .unwrap();
    let acked = s.log_end();
    let local = foreign_state(&s.reader());
    let mut live = Source::new(cited_repo(), link("rl-00001"));
    let first = live.delivery(&[&c.a1, &c.b1], &[], &[]);
    s.set_held(true).unwrap();
    let h = s
        .append_foreign(&first, &[c.a1.clone(), c.b1.clone()])
        .unwrap();
    assert!(s.open_batch().is_some(), "the take's batch is open");
    let killed = d.path().join("killed");
    copy_record(&src, &killed);
    assert!(
        segments(&killed)["seg-000000.log"].len() as u64 > acked.offset,
        "the torn take's frames are on disk"
    );
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    live.head = Some(h);

    let mut k = Store::open(&killed).unwrap();
    // the acknowledged log stays, byte for byte; the torn take is gone, and its first use with it
    assert_eq!(k.log_end(), acked);
    assert_eq!(log_prefix(&killed, acked), log_prefix(&src, acked));
    assert!(!k.features().has(FEATURE_FOREIGN_COPY));
    assert_eq!(foreign_state(&k.reader()), local);
    assert!(k.reader().foreign_sources().unwrap().is_empty());
    // the take again writes what the killed one would have
    let mut again = Source::new(cited_repo(), link("rl-00001"));
    again.deliver(&mut k, &[&c.a1, &c.b1], &[], &[]);
    assert_eq!(again.head, Some(h));
    assert!(k.features().has(FEATURE_FOREIGN_COPY));
    assert_eq!(k.reader().check_objects().unwrap().foreign, 1);
    k.close().unwrap();

    // a follow killed after its first foreign batch keeps that batch and resumes after it
    let mut base = Store::create(&d.path().join("base")).unwrap();
    base.commit_to_view(
        "work",
        change(vec![Op::Create {
            block: block("C-00001", "local claim"),
        }]),
    )
    .unwrap();
    let f = seed(&base, &d.path().join("follower"));
    let mut gone = Source::new(cited_repo(), link("rl-00002"));
    gone.deliver(&mut base, &[&c.d1], &[], &[]);
    let one = base.log_end();
    let after_one = foreign_state(&base.reader());
    gone.revoke(&mut base);
    let (inc, _) = dump(&base, Some(&f.chain));
    let inc_path = d.path().join("inc.rdydump");
    std::fs::write(&inc_path, &inc).unwrap();

    let kill = d.path().join("follow-killed");
    let (from, to) = (f.dir.clone(), kill.clone());
    on_follow_step(Some(Box::new(move |step| {
        if step == FollowStep::Marker(1) {
            copy_record(&from, &to);
        }
    })));
    follow(&f.dir, &inc_path);
    on_follow_step(None);

    let k = Store::open_follower(&kill).unwrap();
    assert_eq!(k.follower_end().unwrap(), one, "the first batch stays");
    assert_eq!(log_prefix(&kill, one), log_prefix(&f.dir, one));
    assert_eq!(foreign_state(&k.reader()), after_one);
    k.close().unwrap();
    // the same increment finishes the follow from there
    follow(&kill, &inc_path);
    let k = Store::open_follower(&kill).unwrap();
    assert_eq!(segments(&kill), segments(&f.dir));
    assert_eq!(foreign_state(&k.reader()), foreign_state(&base.reader()));
}

#[test]
fn rpl11_foreign_bytes_count_revoked_sources() {
    let d = tempfile::tempdir().unwrap();
    let c = cited();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    s.commit_to_view(
        "work",
        change(vec![Op::Create {
            block: block("C-00001", "local claim"),
        }]),
    )
    .unwrap();
    assert_eq!(s.reader().foreign_bytes(&cited_repo()).unwrap(), 0);

    // the bytes of every frame of `kind` from `from` to the end of the log
    let frames = |s: &Store, from: Location, kind: u8| -> u64 {
        let mut sum = 0;
        let r = s.reader();
        r.for_each_batch_after(from, s.log_end(), &mut |b| {
            for f in &b.frames {
                if codec::logical_kind(f.kind()) == kind {
                    sum += HEADER + u64::from(f.original_len());
                }
            }
            Ok(())
        })
        .unwrap();
        sum
    };
    let before = s.log_end();
    let mut live = Source::new(cited_repo(), link("rl-00001"));
    live.deliver(&mut s, &[&c.a1, &c.b1], &[], &[]);
    let mut gone = Source::new(cited_repo(), link("rl-00002"));
    // D-00002 is held already: the second source adds no copy of it
    gone.deliver(&mut s, &[&c.d1, &c.b1], &[], &[]);
    let delivered = frames(&s, before, kinds::FOREIGN) + frames(&s, before, kinds::BLOCK);
    assert_eq!(s.reader().foreign_bytes(&cited_repo()).unwrap(), delivered);

    // a revocation keeps every copy its source holds, and its own entry counts
    gone.revoke(&mut s);
    let revoked = frames(&s, before, kinds::FOREIGN) + frames(&s, before, kinds::BLOCK);
    assert!(revoked > delivered);
    assert_eq!(s.reader().foreign_bytes(&cited_repo()).unwrap(), revoked);
    // a replaced or removed version stays held, and counts
    live.deliver(&mut s, &[], &[&c.a2], &["D-00002"]);
    let all = frames(&s, before, kinds::FOREIGN) + frames(&s, before, kinds::BLOCK);
    assert_eq!(s.reader().foreign_bytes(&cited_repo()).unwrap(), all);
    // another repo's entries and copies are its own
    let mid = s.log_end();
    let mut other = Source::new(other_repo(), link("rl-00003"));
    other.deliver(&mut s, &[&block("D-00009", "of another repo")], &[], &[]);
    let others = frames(&s, mid, kinds::FOREIGN) + frames(&s, mid, kinds::BLOCK);
    assert_eq!(s.reader().foreign_bytes(&cited_repo()).unwrap(), all);
    assert_eq!(s.reader().foreign_bytes(&other_repo()).unwrap(), others);

    // the log byte report counts every FOREIGN frame, the revoked source's included
    let report = s.log_bytes().unwrap();
    let row = report
        .by_kind
        .iter()
        .find(|k| k.kind == kinds::FOREIGN)
        .expect("a foreign row");
    assert_eq!(row.name, "foreign");
    assert_eq!(row.bytes, frames(&s, ORIGIN, kinds::FOREIGN));

    // the counts travel: a load holds the same bytes
    let (bytes, _) = dump(&s, None);
    let to = d.path().join("to");
    load(&bytes, &to);
    let t = Store::open(&to).unwrap();
    assert_eq!(t.reader().foreign_bytes(&cited_repo()).unwrap(), all);
    assert_eq!(t.reader().foreign_bytes(&other_repo()).unwrap(), others);
}
