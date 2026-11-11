//! rpl5 (repo links, D-FK58A): the foreign log. A Delivery or a Revoked entry is one sealed
//! batch with its held BLOCK versions and the ref of its source (`foreign/link/<id>` or
//! `foreign/adr/<name>`). Each source's selection is the fold of its delta chain; the current
//! view of a repo is, per block, the newest version in log order across its live sources. A
//! revoked source keeps its last view and takes no more deliveries. Every delivered version stays
//! readable through a held pin. A reindex rebuilds the refs from the log.
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignId, ForeignPublication, ForeignSource,
    ForeignVersion, selection_identity,
};
use rdy_model::*;
use rdy_store::foreign::{ADR_REF_PREFIX, LINK_REF_PREFIX, source_ref};
use rdy_store::{Store, StoreError, codec, kinds};
use std::collections::BTreeMap;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn cited_repo() -> RepoId {
    RepoId([7; 16])
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
        content: Hash([b.body.len() as u8; 32]),
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

fn identity(sel: &BTreeMap<BlockId, Hash>) -> Hash {
    selection_identity(sel.iter()).unwrap()
}

/// The citing side's own record of a source's selection, to build each delivery's digest.
#[derive(Default)]
struct Source {
    head: Option<Hash>,
    sel: BTreeMap<BlockId, Hash>,
}

impl Source {
    /// A delivery from this source of `added`, `changed` and `removed`, with the digest of the
    /// selection it leaves.
    fn delivery(
        &mut self,
        source: ForeignSource,
        profile: &str,
        added: &[&Block],
        changed: &[&Block],
        removed: &[&str],
    ) -> ForeignEntry {
        for b in added.iter().chain(changed) {
            self.sel.insert(b.id.clone(), version_of(b));
        }
        for r in removed {
            self.sel.remove(&id(r));
        }
        ForeignEntry {
            repo: cited_repo(),
            source,
            event: ForeignEvent::Delivery {
                prev: self.head,
                publication: ForeignPublication {
                    tag: "v1".into(),
                    profile: profile.into(),
                    profile_version: Some(Hash([3; 32])),
                },
                set: None,
                added: added.iter().map(|b| fv(b)).collect(),
                changed: changed.iter().map(|b| fv(b)).collect(),
                removed: removed.iter().map(|r| id(r)).collect(),
                shells: vec![],
                selection: identity(&self.sel),
            },
        }
    }

    /// Takes `entry` with `blocks` and moves this source's head.
    fn take(&mut self, s: &mut Store, entry: &ForeignEntry, blocks: &[Block]) -> Hash {
        let h = s.put_foreign(entry, blocks).unwrap();
        self.head = Some(h);
        h
    }
}

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}

fn versions(view: &BTreeMap<BlockId, rdy_store::foreign::HeldVersion>) -> BTreeMap<String, Hash> {
    view.iter()
        .map(|(k, v)| (k.to_string(), v.version.version))
        .collect()
}

#[test]
fn rpl5_delta_reconstructs_selection() {
    let (_d, mut s) = store();
    let canon_before = s.canon_head().unwrap();
    let a1 = block("D-00001", "first");
    let b1 = block("D-00002", "second");
    let c1 = block("D-00003", "third");
    let mut src = Source::default();
    let e1 = src.delivery(link("rl-00001"), "decisions", &[&a1, &b1, &c1], &[], &[]);
    let batches = s.write_count();
    let h1 = src.take(&mut s, &e1, &[a1.clone(), b1.clone(), c1.clone()]);
    // one sealed batch: the entry, its held versions, its ref and the features ref
    assert_eq!(s.write_count(), batches + 1);
    let frames: Vec<u8> = s.last_batch().iter().map(|(k, _)| *k).collect();
    assert_eq!(
        frames.iter().filter(|k| **k == kinds::FOREIGN).count(),
        1,
        "{frames:?}"
    );
    assert_eq!(
        frames.iter().filter(|k| **k == kinds::BLOCK).count(),
        3,
        "{frames:?}"
    );
    assert!(
        frames
            .iter()
            .all(|k| [kinds::FOREIGN, kinds::BLOCK, kinds::REF_UPDATE].contains(k)),
        "no landing, change or tag joins a delivery: {frames:?}"
    );
    let name = format!("{LINK_REF_PREFIX}rl-00001");
    assert_eq!(source_ref(&e1.source), name);
    assert_eq!(s.get_ref(&name).unwrap(), Some(h1.0.to_vec()));
    // a delivery never moves canon and never lands
    assert_eq!(s.canon_head().unwrap(), canon_before);
    assert!(s.refs_with_prefix("landing").unwrap().is_empty());

    // a delta: one change, one removal, one addition
    let a2 = block("D-00001", "first, revised");
    let d1 = block("D-00004", "fourth");
    let e2 = src.delivery(link("rl-00001"), "decisions", &[&d1], &[&a2], &["D-00002"]);
    let h2 = src.take(&mut s, &e2, &[a2.clone(), d1.clone()]);
    assert_eq!(s.get_ref(&name).unwrap(), Some(h2.0.to_vec()));

    let view = s
        .source_view(&name)
        .unwrap()
        .expect("the source has a view");
    assert_eq!(view.head, h2);
    assert!(!view.revoked);
    assert_eq!(view.repo, cited_repo());
    let want: BTreeMap<String, Hash> = [
        ("D-00001".to_string(), version_of(&a2)),
        ("D-00003".to_string(), version_of(&c1)),
        ("D-00004".to_string(), version_of(&d1)),
    ]
    .into();
    assert_eq!(versions(&view.members), want);
    // the rebuilt selection is the delivery's digest
    let rebuilt: BTreeMap<BlockId, Hash> = view
        .members
        .iter()
        .map(|(k, v)| (k.clone(), v.version.version))
        .collect();
    assert_eq!(identity(&rebuilt), view.selection);
    // the view of the repo is the one live source's
    assert_eq!(versions(&s.repo_view(&cited_repo()).unwrap()), want);

    // a removed member leaves the selection but stays readable through a held pin
    let gone = ForeignId::new(cited_repo(), id("D-00002"));
    assert_eq!(s.held_version(&gone, &version_of(&b1)).unwrap(), Some(b1));
    // the replaced version too
    let first = ForeignId::new(cited_repo(), id("D-00001"));
    assert_eq!(s.held_version(&first, &version_of(&a1)).unwrap(), Some(a1));
    // a version is held for its own id only
    assert_eq!(s.held_version(&first, &version_of(&c1)).unwrap(), None);

    // a digest that the delta does not rebuild is refused, and nothing is written
    let mut bad = src.delivery(link("rl-00001"), "decisions", &[], &[], &["D-00003"]);
    if let ForeignEvent::Delivery { selection, .. } = &mut bad.event {
        *selection = Hash([9; 32]);
    }
    let before = s.write_count();
    assert!(matches!(
        s.put_foreign(&bad, &[]),
        Err(StoreError::Refused(_))
    ));
    assert_eq!(s.write_count(), before);
    assert_eq!(s.get_ref(&name).unwrap(), Some(h2.0.to_vec()));

    // a delivered version needs its BLOCK, with the version's id
    let mut src2 = Source::default();
    let e = src2.delivery(
        link("rl-00009"),
        "decisions",
        &[&block("D-00008", "x")],
        &[],
        &[],
    );
    assert!(matches!(
        s.put_foreign(&e, &[]),
        Err(StoreError::Refused(_))
    ));
    let mut wrong = block("D-00007", "x");
    let mut e = src2.delivery(link("rl-00009"), "decisions", &[&wrong], &[], &[]);
    wrong.id = id("D-00006");
    if let ForeignEvent::Delivery { added, .. } = &mut e.event {
        added[0].version = version_of(&wrong);
    }
    assert!(matches!(
        s.put_foreign(&e, &[wrong]),
        Err(StoreError::Refused(_))
    ));

    // a foreign repo equal to the store's own is refused
    let own = s.repo_id().unwrap().unwrap();
    let mut mine = Source::default().delivery(link("rl-00005"), "decisions", &[], &[], &[]);
    mine.repo = own;
    let err = s.put_foreign(&mine, &[]).unwrap_err();
    assert!(err.to_string().contains("own repo"), "{err}");
    assert_eq!(
        s.get_ref(&format!("{LINK_REF_PREFIX}rl-00005")).unwrap(),
        None
    );
}

#[test]
fn rpl5_sources_overlap_in_log_order() {
    let (_d, mut s) = store();
    let v1 = block("D-00001", "one");
    let v2 = block("D-00001", "two");
    let v3 = block("D-00001", "three");
    let other = block("D-00002", "other");
    let mut a = Source::default();
    let mut b = Source::default();
    let mut x = Source::default();
    let fid = ForeignId::new(cited_repo(), id("D-00001"));

    let ea = a.delivery(link("rl-0000a"), "public", &[&v1, &other], &[], &[]);
    a.take(&mut s, &ea, &[v1.clone(), other.clone()]);
    // a second profile of the same repo, under another link, delivers a newer version
    let eb = b.delivery(link("rl-0000b"), "internal", &[&v2], &[], &[]);
    b.take(&mut s, &eb, std::slice::from_ref(&v2));
    let view = s.repo_view(&cited_repo()).unwrap();
    assert_eq!(view[&id("D-00001")].version.version, version_of(&v2));
    assert_eq!(view[&id("D-00002")].version.version, version_of(&other));
    // each source keeps its own selection
    let va = s.source_view(&source_ref(&ea.source)).unwrap().unwrap();
    assert_eq!(va.members[&id("D-00001")].version.version, version_of(&v1));

    // an ADR source of another repo never enters this repo's view
    let mut ex = x.delivery(adr("adrs"), "adrs", &[&v3], &[], &[]);
    ex.repo = RepoId([8; 16]);
    let hx = s.put_foreign(&ex, std::slice::from_ref(&v3)).unwrap();
    x.head = Some(hx);
    assert_eq!(
        s.get_ref(&format!("{ADR_REF_PREFIX}adrs")).unwrap(),
        Some(hx.0.to_vec())
    );
    let view = s.repo_view(&cited_repo()).unwrap();
    assert_eq!(view[&id("D-00001")].version.version, version_of(&v2));

    // the first source changes the block later in the log: its version is current
    let ea2 = a.delivery(link("rl-0000a"), "public", &[], &[&v3], &[]);
    a.take(&mut s, &ea2, std::slice::from_ref(&v3));
    let view = s.repo_view(&cited_repo()).unwrap();
    assert_eq!(view[&id("D-00001")].version.version, version_of(&v3));
    // the version v3 is held twice (two repos), and stays one object
    assert_eq!(
        s.held_version(&fid, &version_of(&v3)).unwrap(),
        Some(v3.clone())
    );

    // the second source removes it: the first still holds v3
    let eb2 = b.delivery(link("rl-0000b"), "internal", &[], &[], &["D-00001"]);
    b.take(&mut s, &eb2, &[]);
    let view = s.repo_view(&cited_repo()).unwrap();
    assert_eq!(view[&id("D-00001")].version.version, version_of(&v3));
    // the first removes it too: no live source holds it, and a held pin still reads every
    // version
    let ea3 = a.delivery(link("rl-0000a"), "public", &[], &[], &["D-00001"]);
    a.take(&mut s, &ea3, &[]);
    let view = s.repo_view(&cited_repo()).unwrap();
    assert!(!view.contains_key(&id("D-00001")));
    assert!(view.contains_key(&id("D-00002")));
    for v in [&v1, &v2, &v3] {
        assert_eq!(
            s.held_version(&fid, &version_of(v)).unwrap().as_ref(),
            Some(v)
        );
    }
    // a source's prev must be its own head: the other source's head is refused
    let mut cross = a.delivery(link("rl-0000a"), "public", &[], &[], &["D-00002"]);
    if let ForeignEvent::Delivery { prev, .. } = &mut cross.event {
        *prev = b.head;
    }
    assert!(matches!(
        s.put_foreign(&cross, &[]),
        Err(StoreError::Refused(_))
    ));
}

#[test]
fn rpl5_revoke_freezes_reads_and_blocks_delivery() {
    let (_d, mut s) = store();
    let v1 = block("D-00001", "one");
    let w1 = block("D-00002", "two");
    let mut a = Source::default();
    let ea = a.delivery(link("rl-0000a"), "public", &[&v1, &w1], &[], &[]);
    let h1 = a.take(&mut s, &ea, &[v1.clone(), w1.clone()]);
    let name = source_ref(&ea.source);
    let before = s.source_view(&name).unwrap().unwrap();

    // a revocation names the source's head
    let stale = ForeignEntry {
        repo: cited_repo(),
        source: link("rl-0000a"),
        event: ForeignEvent::Revoked {
            prev: Hash([1; 32]),
        },
    };
    assert!(matches!(
        s.put_foreign(&stale, &[]),
        Err(StoreError::Refused(_))
    ));
    let revoke = ForeignEntry {
        repo: cited_repo(),
        source: link("rl-0000a"),
        event: ForeignEvent::Revoked { prev: h1 },
    };
    let batches = s.write_count();
    let hr = s.put_foreign(&revoke, &[]).unwrap();
    assert_eq!(s.write_count(), batches + 1);
    assert_eq!(s.get_ref(&name).unwrap(), Some(hr.0.to_vec()));
    a.head = Some(hr);

    // the revoked source keeps its last view
    let after = s.source_view(&name).unwrap().unwrap();
    assert!(after.revoked);
    assert_eq!(after.head, hr);
    assert_eq!(versions(&after.members), versions(&before.members));
    assert_eq!(after.selection, before.selection);
    // it is no longer a live source of the repo's current view, and its versions stay held
    assert!(s.repo_view(&cited_repo()).unwrap().is_empty());
    let fid = ForeignId::new(cited_repo(), id("D-00001"));
    assert_eq!(
        s.held_version(&fid, &version_of(&v1)).unwrap(),
        Some(v1.clone())
    );

    // no delivery and no second revocation after it, whatever prev it names
    let v2 = block("D-00001", "one, later");
    let count = s.write_count();
    let late = a.delivery(link("rl-0000a"), "public", &[], &[&v2], &[]);
    let err = s.put_foreign(&late, std::slice::from_ref(&v2)).unwrap_err();
    assert!(err.to_string().contains("revoked"), "{err}");
    let mut old_prev = late.clone();
    if let ForeignEvent::Delivery { prev, .. } = &mut old_prev.event {
        *prev = Some(h1);
    }
    assert!(s.put_foreign(&old_prev, std::slice::from_ref(&v2)).is_err());
    let again = ForeignEntry {
        repo: cited_repo(),
        source: link("rl-0000a"),
        event: ForeignEvent::Revoked { prev: hr },
    };
    assert!(
        s.put_foreign(&again, &[])
            .unwrap_err()
            .to_string()
            .contains("revoked")
    );
    // a source with no entry cannot be revoked, and a first delivery names no prev
    let none = ForeignEntry {
        repo: cited_repo(),
        source: link("rl-0000c"),
        event: ForeignEvent::Revoked { prev: h1 },
    };
    assert!(matches!(
        s.put_foreign(&none, &[]),
        Err(StoreError::Refused(_))
    ));
    assert_eq!(s.write_count(), count);
    assert_eq!(s.get_ref(&name).unwrap(), Some(hr.0.to_vec()));
    assert_eq!(s.source_view(&name).unwrap().unwrap().head, hr);
}

#[test]
fn rpl5_reindex_rebuilds_foreign_refs() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    let v1 = block("D-00001", "one");
    let v2 = block("D-00001", "two");
    let w1 = block("D-00002", "w");
    let mut a = Source::default();
    let mut b = Source::default();
    let ea = a.delivery(link("rl-0000a"), "public", &[&v1, &w1], &[], &[]);
    let ha = a.take(&mut s, &ea, &[v1.clone(), w1.clone()]);
    let mut eb = b.delivery(adr("adrs"), "adrs", &[&v2], &[], &[]);
    eb.repo = RepoId([8; 16]);
    b.take(&mut s, &eb, std::slice::from_ref(&v2));
    let ea2 = a.delivery(link("rl-0000a"), "public", &[], &[&v2], &["D-00002"]);
    a.take(&mut s, &ea2, std::slice::from_ref(&v2));
    let revoke = ForeignEntry {
        repo: RepoId([8; 16]),
        source: adr("adrs"),
        event: ForeignEvent::Revoked {
            prev: b.head.unwrap(),
        },
    };
    s.put_foreign(&revoke, &[]).unwrap();

    let refs = s.refs_with_prefix("foreign/").unwrap();
    assert_eq!(refs.len(), 2, "{refs:?}");
    let views = |s: &Store| {
        refs.iter()
            .map(|(name, _)| {
                let v = s.source_view(name).unwrap().unwrap();
                (name.clone(), v.head, v.revoked, versions(&v.members))
            })
            .collect::<Vec<_>>()
    };
    let before = views(&s);
    let repo_before = versions(&s.repo_view(&cited_repo()).unwrap());
    let entries_before = s.count_refs("foreign/").unwrap();

    s.reindex().unwrap();
    assert_eq!(s.refs_with_prefix("foreign/").unwrap(), refs);
    assert_eq!(views(&s), before);
    assert_eq!(versions(&s.repo_view(&cited_repo()).unwrap()), repo_before);
    assert_eq!(s.count_refs("foreign/").unwrap(), entries_before);
    let fid = ForeignId::new(cited_repo(), id("D-00002"));
    assert_eq!(
        s.held_version(&fid, &version_of(&w1)).unwrap(),
        Some(w1.clone())
    );

    // a reopen with no index file rebuilds them from the log too
    s.close().unwrap();
    std::fs::remove_file(dir.join("index.redb")).unwrap();
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.refs_with_prefix("foreign/").unwrap(), refs);
    assert_eq!(views(&s), before);
    // the log was never rewritten: the first entry is still there, as written
    let got: ForeignEntry = s.get_typed(kinds::FOREIGN, &ha).unwrap();
    assert_eq!(got, ea);
}

#[test]
fn rpl5_foreign_refs_are_the_writers_own() {
    let (_d, mut s) = store();
    let err = s
        .set_refs(vec![(format!("{LINK_REF_PREFIX}rl-00001"), vec![0; 32])])
        .unwrap_err();
    assert!(err.to_string().contains("foreign"), "{err}");
    // a removal is not taken through this writer
    let removal = ForeignEntry {
        repo: cited_repo(),
        source: link("rl-00001"),
        event: ForeignEvent::Removal {
            proof: Hash([1; 32]),
            versions: vec![],
        },
    };
    assert!(matches!(
        s.put_foreign(&removal, &[]),
        Err(StoreError::Refused(_))
    ));
}
