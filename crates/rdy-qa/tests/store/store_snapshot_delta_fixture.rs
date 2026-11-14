//! sdl6 (D-918YB, D-8T7PA, D-RP6SM): the frozen SD1 fixture family, the first-use members of
//! the store feature `snapshot_delta` (incompatible, minimum rdy [`SNAPSHOT_DELTA_REQUIRED_RDY`]).
//! A store this rdy creates lists the feature from its first batch, and every landing it writes
//! is one LANDING and its SNAPSHOT_DELTA, with no SNAPSHOT.
//!
//! The members, each written once and never rewritten:
//!
//! - the native store `fixtures/store/snapshot-delta-1/`: `FEATURES`, `FORMAT`, `lock` and one
//!   segment, byte for byte, and no derived index. Its `expect.json` records what an open reads,
//!   the world (the ids, the landings, their deltas and the tags), the inventory, the producer
//!   commit and the minimum rdy of `snapshot_delta`. `published` is false until the release that
//!   first ships `snapshot_delta`, and changes once, before publication.
//! - the dump `fixtures/dump/v4-f1-e1-snapshot-delta.rdydump` of the same project, with its
//!   expect file (`io_feature_fixture` registers the pair in `FIRST_USE`).
//! - the sync batch `fixtures/sync/v2-f1-e1-snapshot-delta.rdysync`, the history of the
//!   project's last tag, with its expect file: the batch's records and a take of it into a new
//!   holder.
use rdy_engine::stream_take::take;
use rdy_engine::{Rdy, Scope};
use rdy_model::{BlockId, Hash, SnapshotDelta};
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::foreign_fixture;
use rdy_store::format::{
    FEATURE_SNAPSHOT_DELTA, REF_STORE_FEATURES, SNAPSHOT_DELTA_REQUIRED_RDY, StoreAccess,
    StoreFeatures, registered_features,
};
use rdy_store::kinds;
use rdy_store::sync_stream::{StreamReader, StreamRecord};
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The blake3 of the inventory text of the native store files, set when they were written. A
/// change here needs a founder decision, and after publication it is never allowed.
const INVENTORY_BLAKE3: &str = "d4f7163f8f968aa820c65878ae5fd82e238a17a2f449eb5d95c061a6a8b81f17";

const DUMP: &str = "v4-f1-e1-snapshot-delta.rdydump";
const DUMP_EXPECT: &str = "v4-f1-e1-snapshot-delta.expect.json";
const SYNC: &str = "v2-f1-e1-snapshot-delta.rdysync";
const SYNC_EXPECT: &str = "v2-f1-e1-snapshot-delta.expect.json";

/// The tags of the world, in order: the first landing's and the last's.
const TAGS: [&str; 2] = ["v1.0.0", "v2.0.0"];

fn qa_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fixture_dir() -> PathBuf {
    qa_dir().join("fixtures/store/snapshot-delta-1")
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn pretty(v: &Value) -> Vec<u8> {
    (serde_json::to_string_pretty(v).unwrap() + "\n").into_bytes()
}

fn ids(w: &rdy_engine::WriteResult) -> BTreeMap<String, String> {
    w.created.clone().into_iter().collect()
}

/// The world the members hold: a doc landed three times (a create, an edit with an addition,
/// a deletion), the first and last landings tagged.
struct World {
    root: tempfile::TempDir,
    doc: String,
    kept: String,
    gone: String,
    landings: Vec<String>,
}

fn build_world() -> World {
    let (root, mut r) = rdy_qa::golden::tiny();
    let w = write(
        &mut r,
        "w0",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"title": "Deltas", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Terms"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s",
             "body": "Each landing stores what changed."}
        ]),
    )
    .unwrap();
    let first = land(&mut r, "w0").unwrap();
    r.tag(&approver(), TAGS[0], None, None).unwrap();
    let doc = ids(&w)["$d"].clone();
    let kept = ids(&w)["$p"].clone();
    let section = ids(&w)["$s"].clone();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "update", "id": kept, "body": "Each landing stores only what changed."},
            {"op": "create", "type": "prose", "id": "$g", "parent": section,
             "body": "A block the next landing removes."}
        ]),
    )
    .unwrap();
    let gone = ids(&w)["$g"].clone();
    let second = land(&mut r, "w1").unwrap();
    write(&mut r, "w2", json!([{"op": "delete", "id": gone}])).unwrap();
    let third = land(&mut r, "w2").unwrap();
    r.tag(&approver(), TAGS[1], None, None).unwrap();
    r.close().unwrap();
    World {
        root,
        doc,
        kept,
        gone,
        landings: vec![first, second, third],
    }
}

/// Each landing of the project at `r` with its delta: `{landing: delta}`.
fn deltas_of(r: &Rdy, landings: &[String]) -> BTreeMap<String, String> {
    landings
        .iter()
        .map(|l| {
            let h: Hash = l.parse().unwrap();
            let (d, _) = r.store().reader().snapshot_delta_of(&h).unwrap().unwrap();
            (l.clone(), d.to_string())
        })
        .collect()
}

fn world_json(w: &World, deltas: &BTreeMap<String, String>) -> Value {
    json!({
        "doc": w.doc,
        "kept": w.kept,
        "gone": w.gone,
        "landings": w.landings,
        "deltas": deltas,
        "tags": TAGS,
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

/// The open facts of the project at `root`: rebuilt index, full fsck, store facts.
fn opened_facts(root: &Path) -> Value {
    let mut r = Rdy::open(root).unwrap();
    assert_eq!(r.store().access(), StoreAccess::ReadWrite);
    r.reindex().unwrap();
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let facts = foreign_fixture::store_facts(&r);
    r.close().unwrap();
    facts
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

/// What the sync batch `bytes` holds, as the released sync expect files state it: the header,
/// each record and the trailer's totals.
fn stream_facts(bytes: &[u8]) -> Value {
    let (mut rd, header) = StreamReader::open(bytes).unwrap();
    let mut records = vec![];
    while let Some(r) = rd.next().unwrap() {
        records.push(match &r {
            StreamRecord::Present { key, payload, .. } => json!({
                "kind": key.kind,
                "hash": key.hash.to_string(),
                "raw_kind": r.raw_kind(),
                "form": "present",
                "len": payload.len(),
            }),
            StreamRecord::Dropped {
                key, original_len, ..
            } => json!({
                "kind": key.kind,
                "hash": key.hash.to_string(),
                "raw_kind": r.raw_kind(),
                "form": "dropped",
                "len": original_len,
            }),
        });
    }
    let totals = rd.finish().unwrap();
    json!({"header": header, "records": records, "totals": totals})
}

/// A take of the sync batch `bytes` (the history of `tag`, at `landing`) into a new holder of
/// its repo: the take's reply and the holder's store facts. The holder's checks pass.
fn take_facts(bytes: &[u8], tag: &str, landing: Hash) -> Value {
    let (_, header) = StreamReader::open(bytes).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("up.rdysync");
    std::fs::write(&p, bytes).unwrap();
    let mut h = Rdy::create_holder(&dir.path().join("holder"), header.repo_id).unwrap();
    let got = take(
        &mut h,
        &[File::open(&p).unwrap()],
        UploadTarget {
            tag: tag.into(),
            landing,
        },
    )
    .unwrap_or_else(|e| panic!("take: {e:?}"));
    h.store().check_holder().unwrap();
    let v = json!({
        "records_taken": got.records_taken,
        "frames_written": got.frames_written,
        "tag": got.tag,
        "tag_hash": got.tag_hash.to_string(),
        "landing": got.landing.to_string(),
        "canon": got.canon.map(|h| h.to_string()),
        "unchanged": got.unchanged,
        "store": foreign_fixture::store_facts(&h),
    });
    h.close().unwrap();
    v
}

/// The landing a tag of the project at `root` names.
fn tag_landing(root: &Path, tag: &str) -> (Hash, Hash) {
    let r = Rdy::open(root).unwrap();
    let (_, t, l) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == tag)
        .map(|(n, t, rec)| (n, t, rec.landing))
        .unwrap();
    r.close().unwrap();
    (t, l)
}

/// Writes the native store, its expect file, the dump, the sync batch and their expect files
/// once, from one world. Run by hand, never by the gate, after the readers fail: `cargo test -p
/// rdy-qa --test store sdl6_write_snapshot_delta_fixtures_once -- --ignored`. It refuses a
/// member that exists.
#[test]
#[ignore]
fn sdl6_write_snapshot_delta_fixtures_once() {
    let dir = fixture_dir();
    let dump = qa_dir().join("fixtures/dump").join(DUMP);
    let dump_expect = qa_dir().join("fixtures/dump").join(DUMP_EXPECT);
    let sync = qa_dir().join("fixtures/sync").join(SYNC);
    let sync_expect = qa_dir().join("fixtures/sync").join(SYNC_EXPECT);
    assert!(!dir.exists(), "snapshot-delta-1 is written once");
    for p in [&dump, &dump_expect, &sync, &sync_expect] {
        assert!(!p.exists(), "{}: written once", p.display());
    }
    let w = build_world();
    let r = Rdy::open(w.root.path()).unwrap();
    let deltas = deltas_of(&r, &w.landings);
    r.close().unwrap();
    let store = w.root.path().join(".rdy/store");
    let copied = foreign_fixture::store_files(&store);
    assert_eq!(
        copied,
        ["FEATURES", "FORMAT", "lock", "segments/seg-000000.log"],
        "one segment"
    );
    for f in &copied {
        write_new(&dir.join(f), &std::fs::read(store.join(f)).unwrap());
    }
    let (_base, root) = foreign_fixture::project_copy(&dir);
    let expect = json!({
        "published": false,
        "producer_commit": producer_commit(),
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": SNAPSHOT_DELTA_REQUIRED_RDY,
        "world": world_json(&w, &deltas),
        "inventory": foreign_fixture::inventory(&dir),
        "store": opened_facts(&root),
    });
    let bytes = pretty(&expect);
    write_new(&dir.join("expect.json"), &bytes);
    eprintln!(
        "INVENTORY_BLAKE3 {}",
        Hash::of(foreign_fixture::inventory_text(&dir).as_bytes())
    );
    eprintln!("expect.json blake3 {}", Hash::of(&bytes));

    let scratch = tempfile::tempdir().unwrap();
    let dumped = scratch.path().join("p.rdydump");
    let r = Rdy::open(w.root.path()).unwrap();
    r.read().dump(&dumped).unwrap();
    r.close().unwrap();
    let dump_bytes = std::fs::read(&dumped).unwrap();
    write_new(&dump, &dump_bytes);
    let v = json!({
        "published": false,
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": SNAPSHOT_DELTA_REQUIRED_RDY,
        "formats": rdy_qa::chain_fixture::current_formats(),
        "world": world_json(&w, &deltas),
        "file": foreign_fixture::dump_file_facts(&dump),
        "whole": foreign_fixture::dump_load_facts(&dump, &scratch.path().join("loaded")),
    });
    let bytes = pretty(&v);
    write_new(&dump_expect, &bytes);
    eprintln!("{DUMP} blake3 {}", Hash::of(&dump_bytes));
    eprintln!("{DUMP_EXPECT} blake3 {}", Hash::of(&bytes));

    // the history of the last tag, exported by the project
    let (t, landing) = tag_landing(w.root.path(), TAGS[1]);
    let r = Rdy::open(w.root.path()).unwrap();
    let batch = rdy_qa::every_kind::export_tag(&r, t);
    r.close().unwrap();
    write_new(&sync, &batch);
    let v = json!({
        "published": false,
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": SNAPSHOT_DELTA_REQUIRED_RDY,
        "formats": rdy_qa::chain_fixture::current_formats(),
        "world": world_json(&w, &deltas),
        "tag": TAGS[1],
        "stream": stream_facts(&batch),
        "taken": take_facts(&batch, TAGS[1], landing),
    });
    let bytes = pretty(&v);
    write_new(&sync_expect, &bytes);
    eprintln!("{SYNC} blake3 {}", Hash::of(&batch));
    eprintln!("{SYNC_EXPECT} blake3 {}", Hash::of(&bytes));
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

/// The names of the features the log's manifest holds.
fn manifest(r: &Rdy) -> Vec<String> {
    let v = r
        .store()
        .reader()
        .get_ref(REF_STORE_FEATURES)
        .unwrap()
        .unwrap();
    StoreFeatures::parse(&v)
        .unwrap()
        .features
        .iter()
        .map(|f| f.name.clone())
        .collect()
}

/// Each recorded landing of `want`'s world has its recorded delta, which names it and folds to
/// its state; no SNAPSHOT is held.
fn assert_world_deltas(r: &Rdy, world: &Value, what: &str) {
    let reader = r.store().reader();
    let mut base = None;
    for l in world["landings"].as_array().unwrap() {
        let l: Hash = l.as_str().unwrap().parse().unwrap();
        let want = world["deltas"][l.to_string()].as_str().unwrap();
        let (d, _) = reader
            .snapshot_delta_of(&l)
            .unwrap()
            .unwrap_or_else(|| panic!("{what}: landing {l} has a delta"));
        assert_eq!(d.to_string(), want, "{what}: delta of {l}");
        let delta: SnapshotDelta = r.store().get_typed(kinds::SNAPSHOT_DELTA, &d).unwrap();
        let rec = r.store().landing(&l).unwrap();
        assert_eq!((delta.landing, delta.full), (l, rec.state), "{what}: {d}");
        assert_eq!(delta.base, base, "{what}: {d} builds on the previous delta");
        assert_eq!(
            r.store().snapshot_at(&l).unwrap().id(),
            rec.state,
            "{what}: {l} folds to its state"
        );
        base = Some(d);
    }
    let objects = foreign_fixture::store_facts(r)["objects"].clone();
    assert!(
        objects.get(kinds::SNAPSHOT.to_string()).is_none(),
        "{what}: no SNAPSHOT is written: {objects}"
    );
}

/// sdl6: the frozen native store opens as written: exactly the four store files and the expect
/// file with the pinned inventory; a copy with no index opens for writing with no upgrade step,
/// rebuilds its index, passes fsck, reads every recorded fact (the landings, their deltas, the
/// tags and both versions of the edited block), survives compaction and a reindex, and the
/// checked-in files never change.
#[test]
fn sdl6_snapshot_delta_fixture_keeps_hashes_landings_and_tags() {
    let dir = fixture_dir();
    assert!(
        dir.join("expect.json").exists(),
        "snapshot-delta-1 is frozen"
    );
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
    assert_eq!(want["required_rdy"], json!(SNAPSHOT_DELTA_REQUIRED_RDY));
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
    // the manifest: the new-store baseline from the first batch (snapshot_delta at its fixed
    // minimum), and what the writes then used
    let reg = registered_features()
        .into_iter()
        .find(|f| f.name == FEATURE_SNAPSHOT_DELTA)
        .unwrap();
    let held = want["store"]["manifest"]["features"].as_array().unwrap();
    for f in rdy_store::format::baseline_features().features {
        assert!(held.contains(&json!(f)), "the manifest holds {}", f.name);
    }
    assert!(held.contains(&json!(reg)));

    // the open of a copy reads back every recorded fact, and plans no upgrade step
    let (_base, root) = foreign_fixture::project_copy(&dir);
    let plan = rdy_store::upgrade::inspect(&root.join(".rdy/store")).unwrap();
    assert!(plan.steps.is_empty(), "no step: {:?}", plan.steps);
    assert!(!plan.requires_upgrade && !plan.pending_check);
    let store = opened_facts(&root);
    assert!(root.join(".rdy/store/index.redb").exists(), "rebuilt");
    for member in want["store"].as_object().unwrap().keys() {
        assert_eq!(store[member], want["store"][member], "{member}");
    }
    let world = &want["world"];
    let mut r = Rdy::open(&root).unwrap();
    assert!(manifest(&r).iter().any(|n| n == FEATURE_SNAPSHOT_DELTA));
    assert_world_deltas(&r, world, "the native store");
    let kept = BlockId::parse(world["kept"].as_str().unwrap()).unwrap();
    let gone = BlockId::parse(world["gone"].as_str().unwrap()).unwrap();
    let landings: Vec<Hash> = world["landings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().parse().unwrap())
        .collect();
    let first = r.read().block(&Scope::At(landings[0]), &kept).unwrap();
    let canon = r.read().block(&Scope::Canon, &kept).unwrap();
    assert_ne!(
        first.version_hash(),
        canon.version_hash(),
        "the edit landed"
    );
    assert!(r.read().block(&Scope::At(landings[1]), &gone).is_ok());
    assert!(
        r.read().block(&Scope::Canon, &gone).is_err(),
        "the deletion landed"
    );
    // compaction and a reindex keep the recorded identities
    r.compact(false).unwrap();
    r.reindex().unwrap();
    assert_eq!(
        foreign_fixture::store_facts(&r)["objects"],
        want["store"]["objects"]
    );
    r.close().unwrap();
    rdy_qa::invariants::check_all(&Rdy::open(&root).unwrap())
        .unwrap_or_else(|v| panic!("invariants: {v:#?}"));
    rdy_qa::reindex_check::reindex_reproduces(&root).unwrap_or_else(|v| panic!("reindex: {v:#?}"));
    assert_eq!(source_digests(), before);
}

/// sdl6: the frozen dump loads into a current store with no upgrade batch: the load's facts are
/// the recorded ones, and every recorded landing has its recorded delta.
#[test]
fn sdl6_snapshot_delta_dump_loads_as_recorded() {
    let dump = qa_dir().join("fixtures/dump").join(DUMP);
    let want = read_json(&qa_dir().join("fixtures/dump").join(DUMP_EXPECT));
    assert_eq!(want["required_rdy"], json!(SNAPSHOT_DELTA_REQUIRED_RDY));
    assert_eq!(foreign_fixture::dump_file_facts(&dump), want["file"]);
    let scratch = tempfile::tempdir().unwrap();
    let to = scratch.path().join("loaded");
    let got = foreign_fixture::dump_load_facts(&dump, &to);
    assert_eq!(got, want["whole"]);
    let r = Rdy::open(&to).unwrap();
    assert_world_deltas(&r, &want["world"], DUMP);
    r.close().unwrap();
}

/// sdl6: the frozen sync batch holds each landing's delta after the root tag, and a take of it
/// into a new holder gives the recorded reply and facts.
#[test]
fn sdl6_snapshot_delta_sync_batch_takes_as_recorded() {
    let bytes = std::fs::read(qa_dir().join("fixtures/sync").join(SYNC)).unwrap();
    let want = read_json(&qa_dir().join("fixtures/sync").join(SYNC_EXPECT));
    assert_eq!(stream_facts(&bytes), want["stream"]);
    let (_, header) = StreamReader::open(bytes.as_slice()).unwrap();
    assert!(header.features.has(FEATURE_SNAPSHOT_DELTA));
    let records = want["stream"]["records"].as_array().unwrap();
    let deltas = records
        .iter()
        .filter(|r| r["kind"] == json!(kinds::SNAPSHOT_DELTA))
        .count();
    assert_eq!(deltas, want["world"]["landings"].as_array().unwrap().len());
    let landing: Hash = want["taken"]["landing"].as_str().unwrap().parse().unwrap();
    let got = take_facts(&bytes, want["tag"].as_str().unwrap(), landing);
    assert_eq!(got, want["taken"]);
}
