//! Ports merge as replays (hub ports). A port's create of a block its target holds is the
//! ordinary `already_exists` conflict, and a port's deps never resolve a conflict. The store
//! keeps no conflict record: every reader derives the conflict from the stored ops alone, so it
//! survives reopen, reindex and a dump loaded into a new store.
use rdy_model::*;
use rdy_store::codec;
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader, DumpWriter, RemovalTally};
use rdy_store::dump_chain::{BatchMarker, DumpChainHeader, batch_end};
use rdy_store::format::{DumpStore, REF_STORE_FEATURES, StoreFeatures};
use rdy_store::ingest::Ingest;
use rdy_store::merge::Side;
use rdy_store::{Store, StoreError, ViewOpKind, kinds};
use std::collections::BTreeMap;
use std::path::Path;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn block(id_: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "claim".into(),
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

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: "someone".into(),
        run: None,
        role,
        at_ms: 1,
    }
}

fn change(view: &str, ops: Vec<Op>, deps: Vec<Hash>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps,
        provenance: prov(Role::Writer),
        justification: None,
        message: "a change".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// `c` as a port of a change of another repo, which no store here holds.
fn port(mut c: Change, n: u8) -> Change {
    c.links.ported_from = Some(PortedFrom {
        repo_id: RepoId([9; 16]),
        change: Hash([n; 32]),
    });
    c
}

/// Lands `b` in canon through view `seed`: the change that created it.
fn seeded(s: &mut Store, b: &Block) -> Hash {
    let c = s
        .commit_to_view(
            "seed",
            change("seed", vec![Op::Create { block: b.clone() }], vec![]),
        )
        .unwrap();
    let p = s.propose("seed", "seed", prov(Role::Writer)).unwrap();
    s.land(&p, prov(Role::Approver)).unwrap();
    c
}

/// Writes `changes` as stored frames, as a prepared port does, and includes them in a new view
/// `name` based on canon's head. The store checks nothing against the view's state here.
fn carry(s: &mut Store, name: &str, changes: &[Change]) -> Vec<Hash> {
    let mut items = vec![];
    for c in changes {
        for op in &c.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op {
                items.push(codec::item_new(kinds::BLOCK, block));
            }
        }
        items.push(codec::item(kinds::CHANGE, c).unwrap());
    }
    s.write(items).unwrap();
    let base = s.canon_head().unwrap();
    s.view_create(name, base, None, false, &prov(Role::Writer))
        .unwrap();
    let ids: Vec<Hash> = changes.iter().map(Change::id).collect();
    s.view_append(
        name,
        vec![ViewOpKind::Include {
            changes: ids.clone(),
        }],
        &prov(Role::Writer),
    )
    .unwrap();
    ids
}

fn side(change: Hash, b: &Block) -> Side {
    Side {
        change: Some(change),
        version: Some(b.version_hash()),
    }
}

fn batches_of(dir: &Path) -> Vec<DumpBatch> {
    let s = Store::open(dir).unwrap();
    let mut out = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            out.push(b);
            Ok(())
        })
        .unwrap();
    out
}

/// The full dump of the store at `src`, loaded into a new store at `to`.
fn dump_and_load(src: &Path, to: &Path) {
    let (repo, manifest) = {
        let s = Store::open(src).unwrap();
        let v = s.reader().get_ref(REF_STORE_FEATURES).unwrap().unwrap();
        (
            s.repo_id().unwrap().unwrap(),
            StoreFeatures::parse(&v).unwrap(),
        )
    };
    let batches = batches_of(src);
    let last = batches.last().unwrap();
    let header = rdy_store::dump::DumpHeader {
        store: DumpStore::Features(manifest.clone()),
        log_end: batch_end(last).unwrap(),
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(repo, BatchMarker::of(last))),
    };
    let mut tally = RemovalTally::default();
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    for b in &batches {
        tally.observe(b).unwrap();
        w.batch(b).unwrap();
    }
    let bytes = w.finish(tally.facts().unwrap()).unwrap().0;
    let (mut reader, _, _) = DumpReader::open(&bytes[..]).unwrap();
    let mut read = vec![];
    while let Some(item) = reader.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            read.push(b);
        }
    }
    let mut i = Ingest::create(to).unwrap();
    for (n, b) in read.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.upgrade(&DumpStore::Features(manifest)).unwrap();
    i.finish().unwrap();
}

/// Runs `check` on the store at `dir` as written, reopened, reindexed, and dumped and loaded
/// into a new store.
fn every_reader(dir: &Path, check: &dyn Fn(&Store, &str)) {
    let mut s = Store::open(dir).unwrap();
    check(&s, "reopen");
    s.reindex().unwrap();
    check(&s, "reindex");
    drop(s);
    let loaded = dir.with_extension("loaded");
    dump_and_load(dir, &loaded);
    let s = Store::open(&loaded).unwrap();
    check(&s, "dump and load");
}

#[test]
fn hpt2_missing_base_conflict_survives_reopen_and_transport() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("t");
    let held = block("b-00001", "the target's own words");
    let (theirs, ported, fresh) = {
        let mut s = Store::create(&dir).unwrap();
        let c0 = seeded(&mut s, &held);
        // a missing-base update stored as a create: one on a block the target holds, one on a
        // block it never held
        let ported = block("b-00001", "the source's words");
        let fresh = block("b-00002", "new to the target");
        let p = port(
            change(
                "carry",
                vec![
                    Op::Create {
                        block: ported.clone(),
                    },
                    Op::Create {
                        block: fresh.clone(),
                    },
                ],
                vec![],
            ),
            1,
        );
        let ids = carry(&mut s, "carry", &[p]);
        ((c0, ids[0]), ported, fresh)
    };
    let (c0, p) = theirs;
    every_reader(&dir, &|s, what| {
        // the stored op is a plain create: no forced-conflict field
        let stored = s.change(&p).unwrap();
        assert!(
            matches!(&stored.ops[0], Op::Create { block } if *block == ported),
            "{what}"
        );
        assert_eq!(stored.links.replayed_from, None, "{what}");
        let m = s.view_merge("carry").unwrap();
        assert_eq!(
            m.conflicts
                .iter()
                .map(|c| (c.block.clone(), c.base, c.sides.clone()))
                .collect::<Vec<_>>(),
            [(held.id.clone(), None, {
                let mut v = vec![side(c0, &held), side(p, &ported)];
                v.sort();
                v
            })],
            "{what}: a create against the held block conflicts; it never replaces the target edit"
        );
        // the never-held block applies; the conflicted one rests at its conflict's base (none)
        assert_eq!(
            m.state.blocks.get(&fresh.id),
            Some(&fresh.version_hash()),
            "{what}"
        );
        assert_eq!(m.state.blocks.get(&held.id), None, "{what}");
        // canon is untouched
        assert_eq!(
            s.canon_state().unwrap().blocks[&held.id],
            held.version_hash(),
            "{what}"
        );
    });
}

#[test]
fn hpr2_create_conflict_survives_every_reader() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("t");
    let held = block("b-00001", "the target's own words");
    let first = block("b-00001", "the source's first words");
    let second = block("b-00001", "the source's second words");
    let (c0, p1, p2) = {
        let mut s = Store::create(&dir).unwrap();
        let c0 = seeded(&mut s, &held);
        let p1 = port(
            change(
                "carry",
                vec![Op::Create {
                    block: first.clone(),
                }],
                vec![],
            ),
            1,
        );
        // the second port depends on the first and on the target's version: an ordinary change
        // with these deps covers every side and resolves; a port only adds its side
        let p2 = port(
            change(
                "carry",
                vec![Op::Update {
                    base: first.version_hash(),
                    block: second.clone(),
                }],
                vec![c0, p1.id()],
            ),
            2,
        );
        let ids = carry(&mut s, "carry", &[p1, p2.clone()]);
        // the same change as an ordinary write resolves the conflict
        let mut plain = p2;
        plain.links.ported_from = None;
        let mut first_plain = port(
            change(
                "plain",
                vec![Op::Create {
                    block: first.clone(),
                }],
                vec![],
            ),
            1,
        );
        first_plain.view = "plain".into();
        plain.view = "plain".into();
        plain.deps = vec![c0, first_plain.id()];
        carry(&mut s, "plain", &[first_plain, plain]);
        (c0, ids[0], ids[1])
    };
    let want = {
        let mut v = vec![side(c0, &held), side(p1, &first), side(p2, &second)];
        v.sort();
        v
    };
    every_reader(&dir, &|s, what| {
        let m = s.view_merge("carry").unwrap();
        assert_eq!(m.conflicts.len(), 1, "{what}");
        let got = &m.conflicts[0];
        assert_eq!(
            (&got.block, got.base, &got.sides),
            (&held.id, None, &want),
            "{what}: the port's deps resolve nothing"
        );
        let plain = s.view_merge("plain").unwrap();
        assert!(plain.conflicts.is_empty(), "{what}: {:?}", plain.conflicts);
        assert_eq!(
            plain.state.blocks.get(&held.id),
            Some(&second.version_hash()),
            "{what}"
        );
        // the landing check reads the same conflict: the port never replaces the target edit
        let changes: Vec<Change> = [p1, p2].iter().map(|h| s.change(h).unwrap()).collect();
        let landed = s.merge_checked(&s.canon_state().unwrap(), &changes);
        assert!(
            matches!(landed, Err(StoreError::AlreadyExists(ref b)) if *b == held.id),
            "{what}: {landed:?}"
        );
    });
}
