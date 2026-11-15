//! scc1 (D-DZDB7, D-R80JP, D-47HVF, D-WWAK7): a holder's canon over real project histories.
//! Two projects push into one holder: a seed, an extend, a hold of a sibling chain, a force onto
//! an unrelated chain and a view head. The frozen first-use members of the store feature
//! `hub_canon` (incompatible, minimum rdy [`HUB_CANON_REQUIRED_RDY`]) hold that holder:
//!
//! - the native holder `fixtures/store/hub-canon-1/`: `FEATURES`, `FORMAT`, `lock` and one
//!   segment, byte for byte, and no derived index. Its `expect.json` records what an open reads,
//!   the world (the landings, the held head, the replaced tip, the view), the inventory, the
//!   producer commit and the minimum rdy of `hub_canon`. `published` is false until the release
//!   that first ships `hub_canon`, and changes once, before publication.
//! - the dump `fixtures/dump/v4-f1-e1-hub-canon.rdydump` of the same holder, with its expect
//!   file (`io_feature_fixture` registers the pair in `FIRST_USE`).
use rdy_engine::Rdy;
use rdy_model::Hash;
use rdy_qa::fixtures::{land, write};
use rdy_qa::foreign_fixture;
use rdy_store::format::{
    FEATURE_HUB_CANON, HUB_CANON_REQUIRED_RDY, REF_STORE_FEATURES, StoreAccess, StoreFeatures,
    registered_features,
};
use rdy_store::hub_canon::{
    CANON_REF, CanonMove, Moved, Replaced, ReplacedKind, held_ref, replaced_ref,
};
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError, codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The blake3 of the inventory text of the native store files, set when they were written. A
/// change here needs a founder decision, and after publication it is never allowed.
const INVENTORY_BLAKE3: &str = "<unset>";

const NATIVE: &str = "hub-canon-1";
const DUMP: &str = "v4-f1-e1-hub-canon.rdydump";
const DUMP_EXPECT: &str = "v4-f1-e1-hub-canon.expect.json";

/// The writer's clock of every move (injected).
const AT: i64 = 1_763_164_800_000;

/// The view the hub keeps.
const VIEW: &str = "w1";

fn qa_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fixture_dir() -> PathBuf {
    qa_dir().join("fixtures/store").join(NATIVE)
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn pretty(v: &Value) -> Vec<u8> {
    (serde_json::to_string_pretty(v).unwrap() + "\n").into_bytes()
}

type Records = Vec<(u8, Vec<u8>)>;

/// The sync frames of the store `s` in `[from, its end)`, in log order: what a push of them
/// brings.
fn frames_since(s: &Store, from: Location) -> Records {
    let mut out = vec![];
    s.reader()
        .for_each_frame_from(from, s.log_end(), |_, raw, _, payload, _| {
            if rdy_store::sync_stream::is_sync_kind(codec::logical_kind(raw)) {
                out.push((raw, payload.to_vec()));
            }
            Ok(())
        })
        .unwrap();
    out
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

/// One push: its frames and the head it names.
struct Push {
    records: Records,
    head: Hash,
}

/// The pushes the world makes, from two projects: `p1` and `p2` on project P's canon, `q2` on
/// a copy of P taken at `p1` (a sibling of `p2`), `r1` on the unrelated project R, and the head
/// of P's view `w1` (all its ops are in P's pushes).
struct Pushes {
    _dirs: Vec<tempfile::TempDir>,
    p1: Push,
    p2: Push,
    q2: Push,
    r1: Push,
    view_head: Hash,
    view_root: Hash,
    repo: rdy_model::RepoId,
}

fn doc(r: &mut Rdy, view: &str, title: &str, body: &str) {
    write(
        r,
        view,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": title, "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "prose", "parent": "$s", "body": body}
        ]),
    )
    .unwrap();
}

fn head_of(r: &Rdy) -> Hash {
    r.store().canon_head().unwrap().unwrap()
}

fn pushes() -> Pushes {
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    // P: p1, then a copy Q at p1, then p2
    let (pd, mut p) = rdy_qa::golden::tiny();
    let repo = p.store().reader().repo_id().unwrap().unwrap();
    doc(&mut p, "w0", "Canon", "The hub keeps one canon.");
    land(&mut p, "w0").unwrap();
    let p1 = Push {
        records: frames_since(p.store(), origin),
        head: head_of(&p),
    };
    let at_p1 = p.store().log_end();
    let root = p.root().to_path_buf();
    p.close().unwrap();
    let qd = tempfile::tempdir().unwrap();
    copy_dir(&root, qd.path());
    let mut p = Rdy::open(&root).unwrap();
    doc(&mut p, VIEW, "Extend", "The canon moves forward.");
    land(&mut p, VIEW).unwrap();
    let p2 = Push {
        records: frames_since(p.store(), at_p1),
        head: head_of(&p),
    };
    let view = p.store().reader().view_head(VIEW).unwrap().unwrap().head;
    let mut root_op = view;
    while let Some(prev) = p.store().reader().view_op(&root_op).unwrap().prev {
        root_op = prev;
    }
    p.close().unwrap();
    // Q: a sibling of p2
    let mut q = Rdy::open(qd.path()).unwrap();
    doc(&mut q, "w2", "Held", "A chain the hub holds beside canon.");
    land(&mut q, "w2").unwrap();
    let q2 = Push {
        records: frames_since(q.store(), at_p1),
        head: head_of(&q),
    };
    q.close().unwrap();
    // R: an unrelated history
    let (rd, mut r) = rdy_qa::golden::tiny();
    doc(&mut r, "w0", "Force", "A history that replaces canon.");
    land(&mut r, "w0").unwrap();
    let r1 = Push {
        records: frames_since(r.store(), origin),
        head: head_of(&r),
    };
    r.close().unwrap();
    Pushes {
        _dirs: vec![pd, qd, rd],
        p1,
        p2,
        q2,
        r1,
        view_head: view,
        view_root: root_op,
        repo,
    }
}

fn mv(
    s: &mut Store,
    push: &Push,
    m: CanonMove,
    expected: Option<Hash>,
) -> Result<Moved, StoreError> {
    let b = s
        .reader()
        .check_move_batch(push.records.clone(), push.head)?;
    s.move_canon(&b, &m, expected, AT)
}

/// The holder `root` after every move of the world: seed p1, extend p2, hold q2, the view
/// head, then force r1.
fn build_holder(root: &Path, w: &Pushes) {
    let mut h = Rdy::create_holder(root, w.repo).unwrap();
    let s = h.store_mut();
    assert_eq!(
        mv(s, &w.p1, CanonMove::Seed, None).unwrap(),
        Moved::Moved { tip: w.p1.head }
    );
    assert_eq!(
        mv(s, &w.p2, CanonMove::Extend, Some(w.p1.head)).unwrap(),
        Moved::Moved { tip: w.p2.head }
    );
    assert_eq!(
        mv(s, &w.q2, CanonMove::Hold, Some(w.p2.head)).unwrap(),
        Moved::Moved { tip: w.q2.head }
    );
    let view = Push {
        records: vec![],
        head: w.view_head,
    };
    let set = CanonMove::SetViewHead {
        name: VIEW.into(),
        root: w.view_root,
    };
    assert_eq!(
        mv(s, &view, set, None).unwrap(),
        Moved::Moved { tip: w.view_head }
    );
    assert_eq!(
        mv(s, &w.r1, CanonMove::Force, Some(w.p2.head)).unwrap(),
        Moved::Moved { tip: w.r1.head }
    );
    h.close().unwrap();
}

fn world_json(w: &Pushes) -> Value {
    json!({
        "canon": w.r1.head.to_string(),
        "landings": [w.p1.head.to_string(), w.p2.head.to_string()],
        "held": w.q2.head.to_string(),
        "replaced": w.p2.head.to_string(),
        "view": VIEW,
        "view_head": w.view_head.to_string(),
        "view_root": w.view_root.to_string(),
        "at_ms": AT,
    })
}

/// The producer commit: the commit the code that wrote the fixture is at.
fn producer_commit() -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git rev-parse HEAD");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A new holder root whose store is a copy of the store files in `from`, with no index. The
/// checked-in files are never opened.
fn holder_copy(from: &Path) -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("h");
    let store = root.join(".rdy/store");
    std::fs::create_dir_all(store.join("segments")).unwrap();
    for f in foreign_fixture::store_files(from) {
        std::fs::copy(from.join(&f), store.join(&f)).unwrap();
    }
    assert!(!store.join("index.redb").exists(), "no index before open");
    (base, root)
}

/// The open facts of the holder at `root`: rebuilt index, the holder check, store facts.
fn opened_facts(root: &Path) -> Value {
    let mut r = Rdy::open_holder(root).unwrap();
    assert_eq!(r.store().access(), StoreAccess::ReadWrite);
    r.reindex().unwrap();
    r.store().check_holder().unwrap();
    let facts = foreign_fixture::store_facts(&r);
    r.close().unwrap();
    facts
}

/// A load of the holder dump `p` into the new holder `to`: the load's counts, then the loaded
/// store's facts after a reindex. The holder check passes.
fn holder_load_facts(p: &Path, to: &Path) -> Value {
    let s = rdy_engine::load::load(p, to).unwrap_or_else(|e| panic!("load: {e:?}"));
    let v = json!({
        "summary": {
            "dump_format": s.dump_format,
            "batches": s.batches,
            "frames": s.frames,
            "landings": s.landings,
            "tags": s.tags,
            "views": s.views,
        },
        "store": opened_facts(to),
    });
    v
}

/// Writes `bytes` to the new file `p`; refuses a file that exists.
fn write_new(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .unwrap_or_else(|e| panic!("{}: written once: {e}", p.display()));
    std::io::Write::write_all(&mut out, bytes).unwrap();
    out.sync_all().unwrap();
}

/// Writes the native holder, its expect file, the dump and its expect file once, from one
/// world. Run by hand, never by the gate, after the readers fail: `cargo test -p rdy-qa --test
/// store scc1_write_hub_canon_fixtures_once -- --ignored`. It refuses a member that exists.
#[test]
#[ignore]
fn scc1_write_hub_canon_fixtures_once() {
    let dir = fixture_dir();
    let dump = qa_dir().join("fixtures/dump").join(DUMP);
    let dump_expect = qa_dir().join("fixtures/dump").join(DUMP_EXPECT);
    assert!(!dir.exists(), "{NATIVE} is written once");
    for p in [&dump, &dump_expect] {
        assert!(!p.exists(), "{}: written once", p.display());
    }
    let w = pushes();
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("holder");
    build_holder(&root, &w);
    let store = root.join(".rdy/store");
    let copied = foreign_fixture::store_files(&store);
    assert_eq!(
        copied,
        ["FEATURES", "FORMAT", "lock", "segments/seg-000000.log"],
        "one segment"
    );
    for f in &copied {
        write_new(&dir.join(f), &std::fs::read(store.join(f)).unwrap());
    }
    let (_base, copy) = holder_copy(&dir);
    let expect = json!({
        "published": false,
        "producer_commit": producer_commit(),
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": HUB_CANON_REQUIRED_RDY,
        "world": world_json(&w),
        "inventory": foreign_fixture::inventory(&dir),
        "store": opened_facts(&copy),
    });
    let bytes = pretty(&expect);
    write_new(&dir.join("expect.json"), &bytes);
    eprintln!(
        "INVENTORY_BLAKE3 {}",
        Hash::of(foreign_fixture::inventory_text(&dir).as_bytes())
    );
    eprintln!("expect.json blake3 {}", Hash::of(&bytes));

    let dumped = scratch.path().join("h.rdydump");
    let r = Rdy::open_holder(&root).unwrap();
    r.read().dump(&dumped).unwrap();
    r.close().unwrap();
    let dump_bytes = std::fs::read(&dumped).unwrap();
    write_new(&dump, &dump_bytes);
    let v = json!({
        "published": false,
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": HUB_CANON_REQUIRED_RDY,
        "formats": rdy_qa::chain_fixture::current_formats(),
        "world": world_json(&w),
        "file": foreign_fixture::dump_file_facts(&dump),
        "whole": holder_load_facts(&dump, &scratch.path().join("loaded")),
    });
    let bytes = pretty(&v);
    write_new(&dump_expect, &bytes);
    eprintln!("{DUMP} blake3 {}", Hash::of(&dump_bytes));
    eprintln!("{DUMP_EXPECT} blake3 {}", Hash::of(&bytes));
}

/// The names of the features the log's manifest holds.
fn manifest(s: &Store) -> Vec<String> {
    let v = s.reader().get_ref(REF_STORE_FEATURES).unwrap().unwrap();
    StoreFeatures::parse(&v)
        .unwrap()
        .features
        .iter()
        .map(|f| f.name.clone())
        .collect()
}

fn hash(v: &Value) -> Hash {
    v.as_str().unwrap().parse().unwrap()
}

/// Every root of `world` stands in the holder `s`: canon at its tip, the held chain and its
/// mark, the replaced tip and its mark (at the injected clock), the view's head, and every
/// landing they name. The holder check passes.
fn assert_roots(s: &Store, world: &Value, what: &str) {
    let r = s.reader();
    let canon = hash(&world["canon"]);
    assert_eq!(r.canon_head().unwrap(), Some(canon), "{what}: canon");
    let held = hash(&world["held"]);
    assert_eq!(
        r.get_ref(&held_ref(&held)).unwrap(),
        Some(vec![]),
        "{what}: held"
    );
    let replaced = hash(&world["replaced"]);
    let mark = r.get_ref(&replaced_ref(&replaced)).unwrap().unwrap();
    assert_eq!(
        Replaced::decode(&mark).unwrap(),
        Replaced {
            kind: ReplacedKind::Landing,
            by: canon,
            at_ms: world["at_ms"].as_i64().unwrap(),
        },
        "{what}: replaced"
    );
    let view = r
        .view_head(world["view"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(view.head, hash(&world["view_head"]), "{what}: view head");
    assert!(view.owner.is_none() && !view.pinned, "{what}: a hub head");
    for l in world["landings"]
        .as_array()
        .unwrap()
        .iter()
        .chain([&world["held"], &world["canon"]])
    {
        assert!(
            r.has_kind(kinds::LANDING, &hash(l)).unwrap(),
            "{what}: landing {l}"
        );
        assert!(
            r.snapshot_delta_of(&hash(l)).unwrap().is_some(),
            "{what}: the delta of {l}"
        );
    }
    assert!(
        manifest(s).iter().any(|n| n == FEATURE_HUB_CANON),
        "{what}: the manifest"
    );
    s.reader().check_holder().unwrap();
}

/// scc1: a holder dump, its load, a reindex and a compaction keep every root of the hub refs:
/// canon, the held chain, the replaced tip and the view, with the same objects; the frozen dump
/// loads as recorded.
#[test]
fn scc1_holder_dump_load_reindex_and_compact_keep_every_root() {
    let w = pushes();
    let world = world_json(&w);
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("holder");
    build_holder(&root, &w);
    let mut h = Rdy::open_holder(&root).unwrap();
    assert_roots(h.store(), &world, "the holder");
    let objects = foreign_fixture::store_facts(&h)["object_keys"].clone();
    // compaction and a reindex
    h.compact(false).unwrap();
    h.reindex().unwrap();
    assert_roots(h.store(), &world, "after compact and reindex");
    assert_eq!(foreign_fixture::store_facts(&h)["object_keys"], objects);
    let dumped = scratch.path().join("h.rdydump");
    h.read().dump(&dumped).unwrap();
    h.close().unwrap();
    // a load of its dump
    let loaded = scratch.path().join("loaded");
    rdy_engine::load::load(&dumped, &loaded).unwrap();
    let mut l = Rdy::open_holder(&loaded).unwrap();
    l.reindex().unwrap();
    assert_roots(l.store(), &world, "the load");
    assert_eq!(foreign_fixture::store_facts(&l)["object_keys"], objects);
    l.compact(false).unwrap();
    l.reindex().unwrap();
    assert_roots(l.store(), &world, "the load after compact and reindex");
    l.close().unwrap();
    // the frozen dump
    let dump = qa_dir().join("fixtures/dump").join(DUMP);
    let want = read_json(&qa_dir().join("fixtures/dump").join(DUMP_EXPECT));
    assert_eq!(want["required_rdy"], json!(HUB_CANON_REQUIRED_RDY));
    assert_eq!(foreign_fixture::dump_file_facts(&dump), want["file"]);
    let to = scratch.path().join("frozen");
    assert_eq!(holder_load_facts(&dump, &to), want["whole"]);
    let f = Rdy::open_holder(&to).unwrap();
    assert_roots(f.store(), &want["world"], DUMP);
    f.close().unwrap();
}

/// The length and blake3 of each fixture file, by relative path.
fn source_digests() -> BTreeMap<String, (u64, String)> {
    foreign_fixture::files_under(&fixture_dir())
        .into_iter()
        .map(|f| {
            let d = foreign_fixture::digest_of(&fixture_dir().join(&f));
            (f, d)
        })
        .collect()
}

/// scc1: the frozen native holder opens as written: exactly the four store files and the
/// expect file with the pinned inventory, the producer commit and the minimum rdy of
/// `hub_canon`; its manifest is the baseline and `hub_canon` as registered; a copy with no index
/// opens for writing with no upgrade step, rebuilds its index, passes the holder check, reads
/// every recorded fact and root, survives compaction and a reindex, and the checked-in files
/// never change.
#[test]
fn scc1_native_fixture_pins_feature_and_minimum() {
    let dir = fixture_dir();
    assert!(dir.join("expect.json").exists(), "{NATIVE} is frozen");
    let before = source_digests();
    let want = read_json(&dir.join("expect.json"));
    assert_eq!(
        foreign_fixture::files_under(&dir),
        [
            "FEATURES",
            "FORMAT",
            "expect.json",
            "lock",
            "segments/seg-000000.log"
        ]
    );
    assert_eq!(want["inventory"], foreign_fixture::inventory(&dir));
    assert_eq!(
        Hash::of(foreign_fixture::inventory_text(&dir).as_bytes()).to_string(),
        INVENTORY_BLAKE3
    );
    assert!(want["published"].is_boolean());
    assert_eq!(want["required_rdy"], json!(HUB_CANON_REQUIRED_RDY));
    assert_eq!(HUB_CANON_REQUIRED_RDY, "0.53.0");
    assert_eq!(
        rdy_store::format::required_rdy(FEATURE_HUB_CANON),
        HUB_CANON_REQUIRED_RDY
    );
    let commit = want["producer_commit"].as_str().unwrap();
    assert!(commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()));
    let text = std::fs::read_to_string(dir.join("expect.json")).unwrap();
    for bad in [
        "/Users",
        "/home/",
        "/tmp",
        "/var/",
        "\\\\",
        ":\\",
        "localhost",
    ] {
        assert!(!text.contains(bad), "the expect file names {bad}");
    }
    // the manifest: the new-store baseline, then hub_canon as registered (and nothing a move
    // did not use)
    let reg = registered_features()
        .into_iter()
        .find(|f| f.name == FEATURE_HUB_CANON)
        .unwrap();
    assert_eq!(reg.required_rdy, HUB_CANON_REQUIRED_RDY);
    let held = want["store"]["manifest"]["features"].as_array().unwrap();
    for f in rdy_store::format::baseline_features().features {
        assert!(held.contains(&json!(f)), "the manifest holds {}", f.name);
    }
    assert!(held.contains(&json!(reg)));
    let features = std::fs::read_to_string(dir.join("FEATURES")).unwrap();
    assert!(features.contains("\"hub_canon\""), "{features}");

    // the open of a copy reads back every recorded fact, and plans no upgrade step
    let (_base, root) = holder_copy(&dir);
    let plan = rdy_store::upgrade::inspect(&root.join(".rdy/store")).unwrap();
    assert!(plan.steps.is_empty(), "no step: {:?}", plan.steps);
    assert!(!plan.requires_upgrade && !plan.pending_check);
    let store = opened_facts(&root);
    assert!(root.join(".rdy/store/index.redb").exists(), "rebuilt");
    for member in want["store"].as_object().unwrap().keys() {
        assert_eq!(store[member], want["store"][member], "{member}");
    }
    let mut r = Rdy::open_holder(&root).unwrap();
    assert_roots(r.store(), &want["world"], "the native holder");
    r.compact(false).unwrap();
    r.reindex().unwrap();
    assert_roots(r.store(), &want["world"], "after compact and reindex");
    assert_eq!(
        foreign_fixture::store_facts(&r)["objects"],
        want["store"]["objects"]
    );
    r.close().unwrap();
    assert_eq!(source_digests(), before);
}

/// Every ref of `s`, its features, its `FORMAT` and `FEATURES` files and its log end.
fn state(s: &Store, dir: &Path) -> Value {
    json!({
        "end": [s.log_end().segment, s.log_end().offset],
        "refs": s
            .reader()
            .refs_with_prefix("")
            .unwrap()
            .into_iter()
            .map(|(n, v)| (n, Hash::of(&v).to_string()))
            .collect::<BTreeMap<_, _>>(),
        "features": manifest(s),
        "format": std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        "features_file": std::fs::read_to_string(dir.join("FEATURES")).unwrap(),
    })
}

/// scc1: a move that fails to append, to sync or to index leaves the objects, the refs, the
/// features, `FORMAT` and the index as they were, through a reopen; the next move then lands
/// whole. A refused chain is `E_BAD_INPUT` and writes nothing; a failed compare is no error.
#[test]
fn scc1_move_faults_leave_only_acknowledged_batches() {
    let w = pushes();
    type Fault = (&'static str, fn(&mut Store));
    let faults: [Fault; 3] = [
        ("append", |s| s.fail_next_append_after(64)),
        ("sync", |s| s.fail_next_sync()),
        ("index", |s| s.fail_next_index_apply()),
    ];
    for (what, fault) in faults {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("holder");
        let store_dir = root.join(".rdy/store");
        let mut h = Rdy::create_holder(&root, w.repo).unwrap();
        // the first move fails: the feature is not listed, nothing is written
        let before = state(h.store(), &store_dir);
        let index = h.store().index_dump_for_tests().unwrap();
        fault(h.store_mut());
        let got = mv(h.store_mut(), &w.p1, CanonMove::Seed, None);
        assert!(got.is_err(), "{what}: {got:?}");
        assert_eq!(state(h.store(), &store_dir), before, "{what}");
        assert_eq!(h.store().index_dump_for_tests().unwrap(), index, "{what}");
        assert!(!format_lists_hub(&store_dir), "{what}: FEATURES");
        h.close().unwrap();
        let mut h = Rdy::open_holder(&root).unwrap();
        assert_eq!(state(h.store(), &store_dir), before, "{what}: reopened");
        // the next move lands whole, the feature in its batch
        assert_eq!(
            mv(h.store_mut(), &w.p1, CanonMove::Seed, None).unwrap(),
            Moved::Moved { tip: w.p1.head },
            "{what}"
        );
        let after = state(h.store(), &store_dir);
        // a later move that fails leaves the moved holder as it stands
        fault(h.store_mut());
        assert!(
            mv(h.store_mut(), &w.p2, CanonMove::Extend, Some(w.p1.head)).is_err(),
            "{what}"
        );
        assert_eq!(state(h.store(), &store_dir), after, "{what}: second");
        h.close().unwrap();
        let h = Rdy::open_holder(&root).unwrap();
        assert_eq!(
            state(h.store(), &store_dir),
            after,
            "{what}: reopened second"
        );
        assert!(format_lists_hub(&store_dir));
        assert_eq!(h.store().canon_head().unwrap(), Some(w.p1.head));
        h.store().check_holder().unwrap();
        h.close().unwrap();
    }
    // a refused chain: E_BAD_INPUT with D-47HVF's reason, nothing written
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("holder");
    let store_dir = root.join(".rdy/store");
    let mut h = Rdy::create_holder(&root, w.repo).unwrap();
    mv(h.store_mut(), &w.p1, CanonMove::Seed, None).unwrap();
    let before = state(h.store(), &store_dir);
    let e = mv(h.store_mut(), &w.r1, CanonMove::Extend, Some(w.p1.head)).unwrap_err();
    let e = rdy_engine::RdyError::from(e);
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(
        e.message,
        format!("the pushed chain does not hold canon tip {}", w.p1.head)
    );
    assert_eq!(state(h.store(), &store_dir), before);
    // a failed compare: the tip it found, no error, nothing written
    assert_eq!(
        mv(h.store_mut(), &w.p2, CanonMove::Extend, Some(w.r1.head)).unwrap(),
        Moved::Behind {
            tip: Some(w.p1.head)
        }
    );
    assert_eq!(state(h.store(), &store_dir), before);
    assert_eq!(
        h.store().reader().get_ref(CANON_REF).unwrap(),
        Some(w.p1.head.0.to_vec())
    );
    h.close().unwrap();
}

fn format_lists_hub(store_dir: &Path) -> bool {
    std::fs::read_to_string(store_dir.join("FEATURES"))
        .unwrap()
        .contains(FEATURE_HUB_CANON)
}
