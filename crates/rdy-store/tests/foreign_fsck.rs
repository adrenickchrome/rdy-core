//! rpl7 (repo links, D-FK58A): fsck proves the foreign log's isolation and provenance. Each
//! source is one chain of earlier entries that a revocation ends, each delivery's selection is
//! the digest its chain rebuilds, each delivered version is a BLOCK of its id, and each foreign
//! pin names delivered content. No record outside the closed list names a foreign entry or a
//! version only a foreign entry holds; a holder and a tag closure take no foreign entry; zeros
//! over a foreign-only version that no authorized Removal names are corruption. Every check runs
//! on the store's index and again on a rebuilt one.
#[path = "support/carried.rs"]
mod carried_fixture;

use carried_fixture::{
    append_raw, block_residue, carried_frames, find, framed_at, record, span, zero,
};
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignId, ForeignParts, ForeignPin,
    ForeignPublication, ForeignSource, ForeignVersion, selection_identity,
};
use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_store::closure::closure;
use rdy_store::stream_error::Reason;
use rdy_store::{Store, StoreError, codec, kinds};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn cited_repo() -> RepoId {
    RepoId([7; 16])
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

fn publication() -> ForeignPublication {
    ForeignPublication {
        tag: "v1".into(),
        profile: "decisions".into(),
        profile_version: None,
    }
}

/// A delivery from `source` after `prev`, of `added` and `changed`, less `removed`, whose
/// selection is `sel` after the delta.
fn delivery(
    source: ForeignSource,
    prev: Option<Hash>,
    sel: &mut BTreeMap<BlockId, Hash>,
    added: &[&Block],
    changed: &[&Block],
    removed: &[&str],
) -> ForeignEntry {
    for b in added.iter().chain(changed) {
        sel.insert(b.id.clone(), version_of(b));
    }
    for r in removed {
        sel.remove(&id(r));
    }
    ForeignEntry {
        repo: cited_repo(),
        source,
        event: ForeignEvent::Delivery {
            prev,
            publication: publication(),
            set: None,
            added: added.iter().map(|b| fv(b)).collect(),
            changed: changed.iter().map(|b| fv(b)).collect(),
            removed: removed.iter().map(|r| id(r)).collect(),
            selection: selection_identity(sel.iter()).unwrap(),
        },
    }
}

/// A store with one link source: a delivery of `a1` and `b1`, then a delivery that changes `a1`
/// to `a2`.
struct Built {
    _dir: tempfile::TempDir,
    path: PathBuf,
    s: Store,
    a1: Block,
    a2: Block,
    b1: Block,
    e1: Hash,
    e2: Hash,
}

fn built() -> Built {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let a1 = block("D-00001", "first");
    let a2 = block("D-00001", "first, revised");
    let b1 = block("D-00002", "second");
    let mut sel = BTreeMap::new();
    let d1 = delivery(link("rl-00001"), None, &mut sel, &[&a1, &b1], &[], &[]);
    let e1 = s.put_foreign(&d1, &[a1.clone(), b1.clone()]).unwrap();
    let d2 = delivery(link("rl-00001"), Some(e1), &mut sel, &[], &[&a2], &[]);
    let e2 = s.put_foreign(&d2, std::slice::from_ref(&a2)).unwrap();
    Built {
        _dir: dir,
        path,
        s,
        a1,
        a2,
        b1,
        e1,
        e2,
    }
}

/// The check's refusal, on the store's index and again on a rebuilt one: both name `want`.
fn refused(s: &mut Store, want: &str) {
    for pass in ["original", "rebuilt"] {
        if pass == "rebuilt" {
            s.reindex().unwrap();
        }
        match s.reader().check_objects() {
            Err(StoreError::Decode(m)) => assert!(m.contains(want), "{pass}: {m:?} lacks {want:?}"),
            other => panic!("{pass}: not a Decode refusal naming {want:?}: {other:?}"),
        }
    }
}

/// The check passes on the store's index and on a rebuilt one, with `entries` foreign entries.
fn clean(s: &mut Store, entries: usize) {
    assert_eq!(s.reader().check_objects().unwrap().foreign, entries);
    s.reindex().unwrap();
    assert_eq!(s.reader().check_objects().unwrap().foreign, entries);
}

/// Writes `entry` as a bare FOREIGN object with no check, and points its source's ref at it.
fn forge(s: &mut Store, entry: &ForeignEntry) -> Hash {
    let h = s.put(kinds::FOREIGN, entry).unwrap();
    let name = rdy_store::foreign::source_ref(&entry.source);
    s.set_refs_unchecked_for_tests(vec![(name, h.0.to_vec())])
        .unwrap();
    h
}

/// A local block that pins `target` at `content`.
fn pinning(target: &Block, content: Hash) -> Block {
    let mut b = block("D-00100", "cites the hub");
    b.foreign = ForeignParts {
        links: BTreeMap::new(),
        pins: [(
            ForeignId::new(cited_repo(), target.id.clone()),
            ForeignPin {
                content,
                encoding: 0,
            },
        )]
        .into(),
    };
    b
}

fn prov() -> Provenance {
    Provenance {
        actor: "human:a".into(),
        run: None,
        role: Role::Approver,
        at_ms: 1,
    }
}

fn change(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: prov(),
        justification: None,
        message: "local".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

#[test]
fn rpl7_fsck_checks_chain_selection_and_pins() {
    // a clean log: two chained deliveries and a revoked second source
    let mut b = built();
    let mut sel = BTreeMap::new();
    let r1 = delivery(link("rl-00002"), None, &mut sel, &[&b.b1], &[], &[]);
    let r1h = b.s.put_foreign(&r1, &[]).unwrap();
    let revoke = ForeignEntry {
        repo: cited_repo(),
        source: link("rl-00002"),
        event: ForeignEvent::Revoked { prev: r1h },
    };
    b.s.put_foreign(&revoke, &[]).unwrap();
    // a local block pins the delivered content of D-00001
    b.s.put(kinds::BLOCK, &pinning(&b.a2, b.a2.content_hash()))
        .unwrap();
    // a pin of the earlier, replaced version still names a held version
    b.s.put(kinds::BLOCK, &pinning(&b.a1, b.a1.content_hash()))
        .unwrap();
    clean(&mut b.s, 4);

    // a pin that names a version hash, not a content hash
    let mut x = built();
    x.s.put(kinds::BLOCK, &pinning(&x.a2, version_of(&x.a2)))
        .unwrap();
    refused(&mut x.s, "which no delivered version holds");

    // a pin of content no delivery holds
    let mut x = built();
    x.s.put(kinds::BLOCK, &pinning(&x.a2, Hash([9; 32])))
        .unwrap();
    refused(&mut x.s, "which no delivered version holds");

    // a selection that is not the digest its chain rebuilds
    let mut x = built();
    let mut sel: BTreeMap<BlockId, Hash> = [(x.a2.id.clone(), version_of(&x.a2))].into();
    let mut bad = delivery(link("rl-00001"), Some(x.e2), &mut sel, &[], &[], &[]);
    if let ForeignEvent::Delivery { selection, .. } = &mut bad.event {
        *selection = selection_identity(sel.iter()).unwrap();
    }
    forge(&mut x.s, &bad);
    refused(&mut x.s, "is not the digest");

    // a delta that removes a block the selection does not hold
    let mut x = built();
    let mut sel: BTreeMap<BlockId, Hash> = [
        (x.a2.id.clone(), version_of(&x.a2)),
        (x.b1.id.clone(), version_of(&x.b1)),
    ]
    .into();
    sel.insert(id("D-00009"), Hash([1; 32]));
    let bad = delivery(
        link("rl-00001"),
        Some(x.e2),
        &mut sel,
        &[],
        &[],
        &["D-00009"],
    );
    forge(&mut x.s, &bad);
    refused(&mut x.s, "removed D-00009 is not in the selection");

    // a prev of another source
    let mut x = built();
    let mut sel = BTreeMap::new();
    let bad = delivery(link("rl-00003"), Some(x.e1), &mut sel, &[], &[], &[]);
    forge(&mut x.s, &bad);
    refused(
        &mut x.s,
        "is not an earlier entry of source foreign/link/rl-00003",
    );

    // an entry after the revocation that ends its source
    let mut x = built();
    let revoke = ForeignEntry {
        repo: cited_repo(),
        source: link("rl-00001"),
        event: ForeignEvent::Revoked { prev: x.e2 },
    };
    let rh = x.s.put_foreign(&revoke, &[]).unwrap();
    let mut sel: BTreeMap<BlockId, Hash> = [
        (x.a2.id.clone(), version_of(&x.a2)),
        (x.b1.id.clone(), version_of(&x.b1)),
    ]
    .into();
    let after = delivery(link("rl-00001"), Some(rh), &mut sel, &[], &[], &[]);
    forge(&mut x.s, &after);
    refused(&mut x.s, "is the revocation that ends source");

    // two entries that follow the same entry
    let mut x = built();
    let mut sel: BTreeMap<BlockId, Hash> = [
        (x.a1.id.clone(), version_of(&x.a1)),
        (x.b1.id.clone(), version_of(&x.b1)),
    ]
    .into();
    let twin = delivery(
        link("rl-00001"),
        Some(x.e1),
        &mut sel,
        &[],
        &[],
        &["D-00002"],
    );
    forge(&mut x.s, &twin);
    refused(&mut x.s, &format!("both follow {}", x.e1));

    // a source ref that does not name its newest entry
    let mut x = built();
    x.s.set_refs_unchecked_for_tests(vec![("foreign/link/rl-00001".into(), x.e1.0.to_vec())])
        .unwrap();
    refused(&mut x.s, "its ref names");

    // a delivered version whose BLOCK is a block of another id
    let mut x = built();
    let other = block("D-00005", "fifth");
    x.s.put(kinds::BLOCK, &other).unwrap();
    let mut sel: BTreeMap<BlockId, Hash> = [
        (x.a2.id.clone(), version_of(&x.a2)),
        (x.b1.id.clone(), version_of(&x.b1)),
    ]
    .into();
    let mut lie = delivery(link("rl-00001"), Some(x.e2), &mut sel, &[], &[], &[]);
    let wrong = ForeignVersion {
        id: id("D-00006"),
        ..fv(&other)
    };
    sel.insert(id("D-00006"), version_of(&other));
    if let ForeignEvent::Delivery {
        added, selection, ..
    } = &mut lie.event
    {
        added.push(wrong);
        *selection = selection_identity(sel.iter()).unwrap();
    }
    forge(&mut x.s, &lie);
    refused(&mut x.s, "its BLOCK is a block of D-00005");

    // an entry of this store's own repo
    let mut x = built();
    let own = x.s.repo_id().unwrap().unwrap();
    let mut sel = BTreeMap::new();
    let mut mine = delivery(link("rl-00004"), None, &mut sel, &[], &[], &[]);
    mine.repo = own;
    forge(&mut x.s, &mine);
    refused(&mut x.s, "is this store's own repo");

    // a Removal of a version no delivery holds
    let mut x = built();
    let removal = ForeignEntry {
        repo: cited_repo(),
        source: link("rl-00001"),
        event: ForeignEvent::Removal {
            proof: Hash([4; 32]),
            versions: vec![(id("D-00001"), Hash([5; 32]))],
        },
    };
    x.s.put(kinds::FOREIGN, &removal).unwrap();
    refused(&mut x.s, "is not a version a delivery of repo");
}

#[test]
fn rpl7_canon_cannot_name_foreign_only_objects() {
    // a local change that updates a foreign-only version
    let mut x = built();
    let mut local = x.a2.clone();
    local.body = "edited here".into();
    x.s.put(
        kinds::CHANGE,
        &change(vec![Op::Update {
            base: version_of(&x.a2),
            block: local,
        }]),
    )
    .unwrap();
    refused(
        &mut x.s,
        &format!(
            "names version {}, which only a foreign entry holds",
            version_of(&x.a2)
        ),
    );

    // a state snapshot that holds a foreign-only version
    let mut x = built();
    let state = StateSnapshot {
        blocks: [(x.b1.id.clone(), version_of(&x.b1))].into(),
    };
    let snap = x.s.put(kinds::SNAPSHOT, &state).unwrap();
    refused(
        &mut x.s,
        &format!(
            "object {}:{snap} names version {}",
            kinds::SNAPSHOT,
            version_of(&x.b1)
        ),
    );

    // a landing whose checks name a foreign entry (a link of any kind)
    let mut x = built();
    landing_with_checks(&mut x.s, vec![x.e2]);
    refused(&mut x.s, &format!("names foreign entry {}", x.e2));

    // the same bytes as local content: a local change created them, so the version is not
    // foreign-only and a local update of it is the project's own history
    let mut x = built();
    let created =
        x.s.put(
            kinds::CHANGE,
            &change(vec![Op::Create {
                block: x.b1.clone(),
            }]),
        )
        .unwrap();
    let mut next = x.b1.clone();
    next.body = "second, locally".into();
    let mut upd = change(vec![Op::Update {
        base: version_of(&x.b1),
        block: next,
    }]);
    upd.deps = vec![created];
    x.s.put(kinds::CHANGE, &upd).unwrap();
    clean(&mut x.s, 2);
}

/// A landing on an empty state, outside canon's chain, whose checks are `checks`.
fn landing_with_checks(s: &mut Store, checks: Vec<Hash>) -> Hash {
    let state = s
        .put(
            kinds::SNAPSHOT,
            &StateSnapshot {
                blocks: BTreeMap::new(),
            },
        )
        .unwrap();
    let proposal = s
        .put(
            kinds::PROPOSAL,
            &Proposal {
                stable: None,
                prev: None,
                title: "empty".into(),
                view: "work".into(),
                changes: vec![],
                alternative: None,
                provenance: prov(),
                canon_at: None,
                summary: None,
            },
        )
        .unwrap();
    s.put(
        kinds::LANDING,
        &LandingRecord {
            proposal,
            prev: None,
            state,
            approved_by: prov(),
            changes: vec![],
            chosen: None,
            checks,
            assurance: Assurance::Local,
            relayed_by: None,
            relay_evidence: None,
            approved_version: None,
            rebased_from: vec![],
            evidence: None,
            assessment: None,
            policy: None,
            read_identity: None,
        },
    )
    .unwrap()
}

#[test]
fn rpl7_holder_refuses_foreign_entries() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = Store::create_holder(&dir.path().join("h"), RepoId([1; 16])).unwrap();
    h.reader().check_holder().unwrap();
    let mut sel = BTreeMap::new();
    let entry = delivery(link("rl-00001"), None, &mut sel, &[], &[], &[]);
    let e = h.put(kinds::FOREIGN, &entry).unwrap();
    for pass in ["original", "rebuilt"] {
        if pass == "rebuilt" {
            h.reindex().unwrap();
        }
        match h.reader().check_holder() {
            Err(StoreError::Decode(m)) => {
                assert_eq!(m, format!("holder holds foreign entry {e}"), "{pass}")
            }
            other => panic!("{pass}: {other:?}"),
        }
    }
    // the writer refuses a foreign entry in a holder too
    assert!(matches!(
        h.put_foreign(&entry, &[]),
        Err(StoreError::Refused(m)) if m == "a holder store holds no foreign entry"
    ));

    // a tag closure, which a holder takes, never carries a foreign entry: an answer of a closure
    // landing that also names one is refused, naming the entry
    let x = built();
    let mut s = x.s;
    let landing = landing_with_checks(&mut s, vec![]);
    let tag = s
        .put(
            kinds::TAG,
            &Tag {
                name: "v1".into(),
                landing,
                message: None,
                tagged_by: prov(),
            },
        )
        .unwrap();
    assert!(closure(&s.reader(), tag, &BTreeSet::new()).is_ok());
    let answer = Answer {
        item: "D-00100@1".into(),
        act: AnswerAct::Confirm,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![landing, x.e2],
        person: prov(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    };
    s.put(kinds::ANSWER, &answer).unwrap();
    let e = closure(&s.reader(), tag, &BTreeSet::new()).unwrap_err();
    assert_eq!(e.code, "E_BAD_RECORD", "{e}");
    assert!(
        e.message.contains(Reason::OutsideTagClosure.as_str()),
        "{e}"
    );
    assert_eq!(
        e.object.map(|k| (k.kind, k.hash)),
        Some((kinds::FOREIGN, x.e2)),
        "{e}"
    );
}

/// The closed store at `path`, with a carried record whose `held: false` span explains zeros over
/// `b`'s BLOCK frame, and those zeros.
fn carried_zeros(path: &Path, b: &Block) {
    let frames = framed_at(path);
    let f = find(&frames, kinds::BLOCK, version_of(b));
    let rec = record(
        vec![RedactTarget::Version {
            id: b.id.clone(),
            version: version_of(b),
        }],
        vec![span(&f, Some(&block_residue(b)), false)],
    );
    append_raw(
        path,
        &carried_frames(&[block_residue(b)], std::slice::from_ref(&rec)),
        true,
    );
    zero(path, &f);
}

#[test]
fn rpl7_unapproved_zeroes_are_corruption() {
    // zeros no record explains: the open refuses them, as for any frame
    let x = built();
    let (path, b1) = (x.path.clone(), x.b1.clone());
    x.s.close().unwrap();
    let frames = framed_at(&path);
    zero(&path, &find(&frames, kinds::BLOCK, version_of(&b1)));
    assert!(matches!(
        Store::open(&path),
        Err(StoreError::RedactionCorrupt { .. })
    ));

    // zeros over a foreign-only version that only a carried record explains: no authorized
    // Removal names them, so they are corruption
    let x = built();
    let (path, b1, dir) = (x.path.clone(), x.b1.clone(), x._dir);
    x.s.close().unwrap();
    carried_zeros(&path, &b1);
    let mut s = Store::open(&path).unwrap();
    let want = format!(
        "foreign version {} of {}/D-00002: its BLOCK reads as removed, and no authorized Removal names it",
        version_of(&b1),
        cited_repo()
    );
    refused(&mut s, &want);

    // a Removal entry that names the version authorizes nothing until its proof is verified
    let removal = ForeignEntry {
        repo: cited_repo(),
        source: link("rl-00001"),
        event: ForeignEvent::Removal {
            proof: Hash([4; 32]),
            versions: vec![(b1.id.clone(), version_of(&b1))],
        },
    };
    s.put(kinds::FOREIGN, &removal).unwrap();
    refused(&mut s, &want);
    drop(s);
    drop(dir);

    // the same zeros over a version that is also local content: the local removal stands
    let mut x = built();
    x.s.put(
        kinds::CHANGE,
        &change(vec![Op::Create {
            block: x.b1.clone(),
        }]),
    )
    .unwrap();
    let (path, b1, _dir) = (x.path.clone(), x.b1.clone(), x._dir);
    x.s.close().unwrap();
    carried_zeros(&path, &b1);
    let mut s = Store::open(&path).unwrap();
    clean(&mut s, 2);
}
