//! scc1 (D-QCE8M, D-NME32): `landed` follows current canon. A change is landed when a landing
//! on canon's chain carries it, or carries a change whose `links.replayed_from` chain reaches
//! it, a view replay's included. `landing_of(c)` is the oldest such landing; a chain that left
//! canon lands nothing. The index derives the rows from canon, and an index an earlier rdy wrote
//! or moved is rebuilt from the log once.
use rdy_model::*;
use rdy_store::Store;
use rdy_store::index::{Index, IndexEntry};
use rdy_store::segment::Location;
use std::collections::BTreeMap;
use std::path::Path;

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
        foreign: Default::default(),
        read_identity: None,
    }
}

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: "agent:a".into(),
        run: Some("r".into()),
        role,
        at_ms: 7,
    }
}

fn create(view: &str, id: &str, body: &str) -> Change {
    Change {
        view: view.into(),
        ops: vec![Op::Create {
            block: block(id, body),
        }],
        deps: vec![],
        provenance: prov(Role::Writer),
        justification: None,
        message: "m".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// A replay of `source` into view `view`, in a new view of that name: one change that creates
/// block `id`, with `links.replayed_from` set to `source`.
fn replay(s: &mut Store, view: &str, source: Hash, id: &str, body: &str) -> Hash {
    s.view_create(view, None, None, false, &prov(Role::Writer))
        .unwrap();
    let mut c = create(view, id, body);
    c.links.replayed_from = Some(source);
    let (_, ids) = s
        .view_replay_prepared(view, &[c], &prov(Role::Writer))
        .unwrap();
    ids[0]
}

fn land(s: &mut Store, view: &str) -> Hash {
    let p = s.propose(view, view, prov(Role::Writer)).unwrap();
    s.land(&p, prov(Role::Approver)).unwrap()
}

/// A project store: change `c` in view `a` (in flight), its replay `r1` in view `b`, and the
/// replay of `r1`, `r2`, in view `c`.
struct Replays {
    c: Hash,
    r1: Hash,
    r2: Hash,
}

fn replays(s: &mut Store) -> Replays {
    let c = s
        .commit_to_view("a", create("a", "b-00001", "one"))
        .unwrap();
    let r1 = replay(s, "b", c, "b-00001", "one");
    let r2 = replay(s, "c", r1, "b-00002", "two");
    Replays { c, r1, r2 }
}

#[test]
fn scc1_landed_membership_follows_canon_and_replay() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let base = s
        .commit_to_view("seed", create("seed", "b-00009", "seed"))
        .unwrap();
    let l0 = land(&mut s, "seed");
    let x = replays(&mut s);
    // D-EHC2C: the replay op itself changes neither its source view nor canon
    for h in [x.c, x.r1, x.r2] {
        assert_eq!(s.landing_of(&h).unwrap(), None, "{h} is in flight");
    }
    assert_eq!(s.included("a").unwrap().as_ref(), &vec![x.c]);
    // the landing of b carries r1, a view replay of c: both read landed there
    let l1 = land(&mut s, "b");
    assert_eq!(s.landing_of(&x.r1).unwrap(), Some(l1));
    assert_eq!(s.landing_of(&x.c).unwrap(), Some(l1), "the replay's source");
    assert_eq!(s.landing_of(&x.r2).unwrap(), None);
    assert_eq!(s.landing_of(&base).unwrap(), Some(l0));
    // the landing of c carries r2, whose chain reaches r1 and c: each keeps its oldest carrier
    let l2 = land(&mut s, "c");
    assert_eq!(s.landing_of(&x.r2).unwrap(), Some(l2));
    assert_eq!(s.landing_of(&x.r1).unwrap(), Some(l1));
    assert_eq!(s.landing_of(&x.c).unwrap(), Some(l1));
    // view a still holds c, and c reads landed: it cannot land again
    assert_eq!(s.included("a").unwrap().as_ref(), &vec![x.c]);
    let p = s.propose("a", "a", prov(Role::Writer));
    let again = p.and_then(|p| s.land(&p, prov(Role::Approver)));
    assert!(
        matches!(&again, Err(e) if e.to_string().contains(&format!("change {} already landed in {l1}", x.c))),
        "{again:?}"
    );
    // fsck reads the same membership from canon's records, and a reindex reproduces the rows
    s.check_objects().unwrap();
    let before = s.index_dump_for_tests().unwrap();
    s.reindex().unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), before);
    assert_eq!(s.landing_of(&x.c).unwrap(), Some(l1));
    // a row canon does not give is damage that fsck reports
    s.set_landed_raw_for_tests(&x.c, &l2.0).unwrap();
    let text = s.check_objects().unwrap_err().to_string();
    assert!(
        text.contains(&format!(
            "landing {l1}: the index has {:?} as the landing of change {}; run rdy reindex",
            Some(l2),
            x.c
        )),
        "{text}"
    );
    s.reindex().unwrap();
    let extra = Hash([0x5A; 32]);
    s.set_landed_raw_for_tests(&extra, &l0.0).unwrap();
    let text = s.check_objects().unwrap_err().to_string();
    assert!(
        text.contains(&format!(
            "the index has landing {l0} as the landing of change {extra}, which canon does not carry; run rdy reindex"
        )),
        "{text}"
    );
    s.reindex().unwrap();
    s.check_objects().unwrap();
}

/// A hash for test label `label`.
fn h(label: &str) -> Hash {
    Hash::of(label.as_bytes())
}

fn carries(landing: &str, prev: Option<&str>, changes: &[&str]) -> IndexEntry {
    IndexEntry::LandingCarries {
        landing: h(landing),
        prev: prev.map(h),
        changes: changes.iter().map(|c| h(c)).collect(),
    }
}

fn replays_of(change: &str, source: &str) -> IndexEntry {
    IndexEntry::Replay {
        change: h(change),
        source: h(source),
    }
}

fn canon(landing: &str) -> IndexEntry {
    IndexEntry::Ref("canon".into(), h(landing).0.to_vec())
}

fn at(n: u64) -> Location {
    Location {
        segment: 0,
        offset: n,
    }
}

#[test]
fn scc1_landing_of_returns_oldest_current_canon_carrier() {
    let dir = tempfile::tempdir().unwrap();
    let ix = Index::open(&dir.path().join("index.redb")).unwrap();
    let of = |c: &str| ix.landing_of(&h(c)).unwrap();
    // l1 carries a direct change and r1, a replay of s; l2 carries r2, which replays r1 (so it
    // reaches s); l3 carries r3, a second replay of s held in another view; no landing carries t
    ix.apply(
        &[
            replays_of("r1", "s"),
            replays_of("r2", "r1"),
            replays_of("r3", "s"),
            carries("l1", None, &["a", "r1"]),
            canon("l1"),
        ],
        at(10),
    )
    .unwrap();
    assert_eq!(of("a"), Some(h("l1")), "a direct carrier");
    assert_eq!(of("r1"), Some(h("l1")));
    assert_eq!(of("s"), Some(h("l1")), "a replay carrier");
    assert_eq!(of("t"), None);
    ix.apply(
        &[
            carries("l2", Some("l1"), &["r2", "b"]),
            canon("l2"),
            carries("l3", Some("l2"), &["r3"]),
            canon("l3"),
        ],
        at(20),
    )
    .unwrap();
    // several landings carry s and r1: the oldest on canon's chain is theirs
    assert_eq!(of("r2"), Some(h("l2")));
    assert_eq!(of("b"), Some(h("l2")));
    assert_eq!(of("r3"), Some(h("l3")));
    assert_eq!(of("r1"), Some(h("l1")), "transitive, oldest");
    assert_eq!(of("s"), Some(h("l1")), "two views' replays, oldest");
    // canon moves to a chain that forks after l1: l2 and l3 are replaced and land nothing
    ix.apply(
        &[
            replays_of("r4", "r3"),
            carries("x2", Some("l1"), &["d", "r4"]),
            canon("x2"),
        ],
        at(30),
    )
    .unwrap();
    for c in ["r2", "b"] {
        assert_eq!(of(c), None, "{c}: its only carrier left canon");
    }
    assert_eq!(of("d"), Some(h("x2")));
    assert_eq!(of("r4"), Some(h("x2")));
    assert_eq!(of("r3"), Some(h("x2")), "reached through r4 now");
    assert_eq!(of("r1"), Some(h("l1")));
    assert_eq!(of("s"), Some(h("l1")));
    assert_eq!(of("a"), Some(h("l1")));
    // a chain that shares no landing with canon replaces all of it; a held or replaced chain
    // alone lands nothing
    ix.apply(&[carries("y1", None, &["e"]), canon("y1")], at(40))
        .unwrap();
    assert_eq!(of("e"), Some(h("y1")));
    for c in ["a", "r1", "s", "d", "r3", "r4", "r2", "b"] {
        assert_eq!(of(c), None, "{c}");
    }
    // back to l3: the rows are the ones a walk from canon's head gives
    ix.apply(&[canon("l3")], at(50)).unwrap();
    for (c, l) in [
        ("a", "l1"),
        ("r1", "l1"),
        ("s", "l1"),
        ("r2", "l2"),
        ("b", "l2"),
        ("r3", "l3"),
    ] {
        assert_eq!(of(c), Some(h(l)), "{c}");
    }
    for c in ["e", "d", "r4"] {
        assert_eq!(of(c), None, "{c}");
    }
}

/// The landed rows an index an earlier rdy wrote would hold for `changes`: none. The index
/// loses `changes`' rows and the mark of this rdy's landed rows; then an index left so reads as
/// an earlier rdy's.
fn as_earlier_rdy_wrote(store: &Path, changes: &[Hash]) {
    let ix = Index::open(&store.join("index.redb")).unwrap();
    for c in changes {
        ix.remove_landed_for_tests(c).unwrap();
    }
    ix.forget_landed_revision_for_tests().unwrap();
}

/// What an older rdy's write leaves: `indexed_upto` moves past the landed rows' mark, here back
/// at `mark`, and the rows it wrote follow its own rule (`changes` lose theirs).
fn as_older_rdy_moved(store: &Path, mark: Location, changes: &[Hash]) {
    let ix = Index::open(&store.join("index.redb")).unwrap();
    for c in changes {
        ix.remove_landed_for_tests(c).unwrap();
    }
    ix.set_landed_upto_for_tests(mark).unwrap();
}

fn kept_aside(store: &Path) -> Vec<String> {
    std::fs::read_dir(store)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("index.redb") && n != "index.redb")
        .collect()
}

#[test]
fn scc1_old_index_rebuilds_once_and_tracks_older_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let x = replays(&mut s);
    let l1 = land(&mut s, "b");
    assert_eq!(s.landing_of(&x.c).unwrap(), Some(l1));
    s.close().unwrap();

    // an index an earlier rdy wrote: its rows know no replay, and it has no revision
    as_earlier_rdy_wrote(&path, &[x.c]);
    let s = Store::open(&path).unwrap();
    assert!(
        s.open_report().index_rebuilt,
        "the earlier index is rebuilt"
    );
    assert_eq!(s.landing_of(&x.c).unwrap(), Some(l1));
    assert_eq!(
        kept_aside(&path),
        Vec::<String>::new(),
        "no quarantine file"
    );
    s.check_objects().unwrap();
    s.close().unwrap();
    // once: the rebuilt index is current
    let mut s = Store::open(&path).unwrap();
    assert!(!s.open_report().index_rebuilt, "rebuilt once");

    // an older rdy writes to the store: its write moves `indexed_upto` but not the mark
    let mark = s.log_end();
    let l2 = land(&mut s, "c");
    assert_eq!(s.landing_of(&x.r2).unwrap(), Some(l2));
    s.close().unwrap();
    as_older_rdy_moved(&path, mark, &[x.r2]);
    let s = Store::open(&path).unwrap();
    assert!(s.open_report().index_rebuilt, "an older write is caught");
    assert_eq!(s.landing_of(&x.r2).unwrap(), Some(l2));
    assert_eq!(s.landing_of(&x.c).unwrap(), Some(l1));
    assert_eq!(
        kept_aside(&path),
        Vec::<String>::new(),
        "no quarantine file"
    );
    s.check_objects().unwrap();
    s.close().unwrap();
    let s = Store::open(&path).unwrap();
    assert!(!s.open_report().index_rebuilt, "rebuilt once");
    s.close().unwrap();
}
