//! rfr2 (D-MKSFF, D-3DAX4): a figure's dark image in the store. Both images stay reachable
//! through the block and change walkers, tags, compaction and reindex; the store feature
//! `figure_dark` (incompatible, minimum rdy [`FIGURE_DARK_REQUIRED_RDY`]) turns on in the same
//! sealed batch as the first record that holds a dark image, in any view, from any write path,
//! and never before; and the frozen native fixture `fixtures/store/figure-dark-1/` opens as
//! written.
//!
//! The fixture members, each written once and never rewritten:
//!
//! - the native store `fixtures/store/figure-dark-1/`: `FEATURES`, `FORMAT`, `lock` and one
//!   segment, byte for byte, and no derived index. Its `expect.json` records what an open reads,
//!   the world (the ids, both blob hashes, the landing and the tag), the inventory, the producer
//!   commit and the minimum rdy of `figure_dark`. `published` is false until the release that
//!   first ships `figure_dark`, and changes once, before publication.
//! - the dump `fixtures/dump/v4-f1-e1-figure-dark.rdydump` of the same project, with its expect
//!   file (`release_dark_figure_formats` and `io_feature_fixture` read them).
use rdy_engine::{Rdy, Scope};
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::foreign_fixture;
use rdy_store::format::{
    FEATURE_FIGURE_DARK, FIGURE_DARK_REQUIRED_RDY, FeatureClass, REF_STORE_FEATURES, StoreAccess,
    StoreFeatures, read_format_file, registered_features,
};
use rdy_store::kinds;
use rdy_store::object_refs::refs_of;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The blake3 of the inventory text of the native store files, set when they were written. A
/// change here needs a founder decision, and after publication it is never allowed.
const INVENTORY_BLAKE3: &str = "";

const DUMP: &str = "v4-f1-e1-figure-dark.rdydump";
const DUMP_EXPECT: &str = "v4-f1-e1-figure-dark.expect.json";

const LIGHT: &[u8] = b"\x89PNG\r\n\x1a\nlight figure image";
const DARK: &[u8] = b"\x89PNG\r\n\x1a\ndark figure image";

fn qa_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fixture_dir() -> PathBuf {
    qa_dir().join("fixtures/store/figure-dark-1")
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

/// One sealed batch, as this test reads it: whether it moves the feature manifest to one that
/// holds `figure_dark`, and the blobs its records name.
struct Batch {
    turns_on: bool,
    blobs: Vec<Hash>,
}

fn batches(r: &Rdy) -> Vec<Batch> {
    let mut out = vec![];
    let mut cur = Batch {
        turns_on: false,
        blobs: vec![],
    };
    let origin = rdy_store::segment::Location {
        segment: 0,
        offset: 0,
    };
    r.store()
        .for_each_frame_from(origin, r.store().log_end(), |_, kind, _, payload, _| {
            if kind == kinds::BATCH_COMMIT {
                out.push(std::mem::replace(
                    &mut cur,
                    Batch {
                        turns_on: false,
                        blobs: vec![],
                    },
                ));
                return Ok(());
            }
            if rdy_store::codec::logical_kind(kind) == kinds::REF_UPDATE {
                let u: kinds::RefUpdate = postcard::from_bytes(payload).unwrap();
                if u.name == REF_STORE_FEATURES {
                    cur.turns_on |= StoreFeatures::parse(&u.value)
                        .unwrap()
                        .features
                        .iter()
                        .any(|f| f.name == FEATURE_FIGURE_DARK);
                }
                return Ok(());
            }
            for o in refs_of(kind, payload).unwrap_or_default() {
                if o.kind == Some(kinds::BLOB_MANIFEST) {
                    cur.blobs.push(o.hash);
                }
            }
            Ok(())
        })
        .unwrap();
    // frames after the last marker (blob uploads) form a group of their own
    out.push(cur);
    out
}

/// The feature turns on exactly once, in the first batch whose records name the dark blob
/// `dark`, and FORMAT mirrors the log.
fn assert_first_use(r: &Rdy, root: &Path, dark: &Hash, what: &str) {
    let all = batches(r);
    let first = all
        .iter()
        .position(|b| b.blobs.contains(dark))
        .unwrap_or_else(|| panic!("{what}: no batch names the dark blob"));
    let on: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, b)| b.turns_on)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        on,
        vec![first],
        "{what}: the feature joins the first use's batch"
    );
    assert!(
        manifest(r).contains(&FEATURE_FIGURE_DARK.to_string()),
        "{what}"
    );
    let mirror = read_format_file(&root.join(".rdy/store")).unwrap().unwrap();
    assert!(
        mirror
            .features
            .iter()
            .any(|f| f.name == FEATURE_FIGURE_DARK),
        "{what}: FORMAT mirrors the log"
    );
}

fn assert_off(r: &Rdy, what: &str) {
    assert!(
        !manifest(r).contains(&FEATURE_FIGURE_DARK.to_string()),
        "{what}"
    );
    assert!(batches(r).iter().all(|b| !b.turns_on), "{what}");
}

fn doc_ops() -> Value {
    json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "figs", "title": "Figures", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Pictures"}}
    ])
}

fn figure_op(parent: &str, light: &Hash, dark: Option<&Hash>) -> Value {
    let mut fields = json!({"blob": light.to_string(), "media_type": "image/png", "alt": "A loop"});
    if let Some(d) = dark {
        fields["dark_blob"] = json!(d.to_string());
        fields["dark_media_type"] = json!("image/png");
    }
    json!({"op": "create", "type": "figure", "id": "$f", "parent": parent, "fields": fields})
}

/// A new project with a doc and a section in canon, and both images in the store.
fn base() -> (tempfile::TempDir, Rdy, String, Hash, Hash) {
    let (d, mut r) = rdy_qa::golden::tiny();
    let w = write(&mut r, "w0", doc_ops()).unwrap();
    land(&mut r, "w0").unwrap();
    let light = r.store_mut().put_blob(LIGHT).unwrap();
    let dark = r.store_mut().put_blob(DARK).unwrap();
    (d, r, ids(&w)["$s"].clone(), light, dark)
}

#[test]
fn rfr2_feature_activates_in_the_first_durable_batch() {
    // the registry: incompatible, with its own fixed minimum, never read-only compatible
    let reg = registered_features();
    let f = reg
        .iter()
        .find(|f| f.name == FEATURE_FIGURE_DARK)
        .expect("figure_dark is registered");
    assert_eq!(f.class, FeatureClass::Incompatible);
    assert_eq!(f.required_rdy, FIGURE_DARK_REQUIRED_RDY);
    assert_eq!(FIGURE_DARK_REQUIRED_RDY, "0.51.0");
    assert_eq!(reg.len(), 9);

    // a store with no dark figure never turns it on: light figures, edits, landings, tags
    let (d, mut r, s, light, _dark) = base();
    let w = write(&mut r, "a", json!([figure_op(&s, &light, None)])).unwrap();
    let fig = ids(&w)["$f"].clone();
    write(
        &mut r,
        "a",
        json!([{"op": "update", "id": fig, "fields": {"alt": "Edited"}}]),
    )
    .unwrap();
    land(&mut r, "a").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    assert_off(&r, "light only");
    drop(d);

    // a CHANGE create in a view
    let (d, mut r, s, light, dark) = base();
    write(&mut r, "a", json!([figure_op(&s, &light, Some(&dark))])).unwrap();
    assert_first_use(&r, d.path(), &dark, "create in a view");
    // later writes in other views, the landing (BLOCK records) and a replay add no second move
    write(&mut r, "b", json!([figure_op(&s, &light, Some(&dark))])).unwrap();
    land(&mut r, "a").unwrap();
    assert_first_use(&r, d.path(), &dark, "after the landing");
    r.close().unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_first_use(&r, d.path(), &dark, "after a reopen");
    drop((r, d));

    // a CHANGE update that adds the dark image to a light figure, in a second view
    let (d, mut r, s, light, dark) = base();
    let w = write(&mut r, "a", json!([figure_op(&s, &light, None)])).unwrap();
    let fig = ids(&w)["$f"].clone();
    land(&mut r, "a").unwrap();
    assert_off(&r, "before the update");
    write(
        &mut r,
        "other",
        json!([{"op": "update", "id": fig, "fields": {"dark_blob": dark.to_string(), "dark_media_type": "image/png"}}]),
    )
    .unwrap();
    assert_first_use(&r, d.path(), &dark, "update in another view");
    // the feature never turns off: the dark image removed again, and landed
    write(
        &mut r,
        "other",
        json!([{"op": "update", "id": fig, "unset": ["dark_blob", "dark_media_type"]}]),
    )
    .unwrap();
    land(&mut r, "other").unwrap();
    assert_first_use(&r, d.path(), &dark, "after removal");
    drop((r, d));

    // an import into a held batch: the feature and the records in one batch
    let (d, mut r, s, light, dark) = base();
    write(&mut r, "a", json!([figure_op(&s, &light, Some(&dark))])).unwrap();
    land(&mut r, "a").unwrap();
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("export");
    r.read().export(&Scope::Canon, &out).unwrap();
    let (d2, mut r2) = rdy_qa::golden::tiny();
    assert_off(&r2, "before the import");
    r2.import(&approver(), "import", &out).unwrap();
    assert_first_use(&r2, d2.path(), &dark, "import");
    drop((r, d, r2, d2));

    // a refused write turns nothing on: a dark figure whose dark blob is not in the store
    let (_d, mut r, s, light, _dark) = base();
    let never = rdy_store::Store::blob_hash(b"never stored");
    let end = r.store().log_end();
    assert!(write(&mut r, "a", json!([figure_op(&s, &light, Some(&never))])).is_err());
    assert_eq!(r.store().log_end(), end);
    assert_off(&r, "refused write");
}

#[test]
fn rfr2_both_variants_remain_reachable() {
    let (d, mut r, s, light, dark) = base();
    let w = write(&mut r, "a", json!([figure_op(&s, &light, Some(&dark))])).unwrap();
    let fig = BlockId::parse(&ids(&w)["$f"]).unwrap();
    let landing: Hash = land(&mut r, "a").unwrap().parse().unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();

    // the walkers: the change of the write and the landed block each name both blobs once
    let mut block_refs = vec![];
    let mut change_refs = vec![];
    r.store()
        .for_each_frame_from(
            rdy_store::segment::Location {
                segment: 0,
                offset: 0,
            },
            r.store().log_end(),
            |_, kind, _, payload, _| {
                let blobs: Vec<Hash> = refs_of(kind, payload)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|o| o.kind == Some(kinds::BLOB_MANIFEST))
                    .map(|o| o.hash)
                    .collect();
                if blobs.contains(&dark) {
                    match rdy_store::codec::logical_kind(kind) {
                        kinds::BLOCK => block_refs.push(blobs),
                        kinds::CHANGE => change_refs.push(blobs),
                        _ => {}
                    }
                }
                Ok(())
            },
        )
        .unwrap();
    let mut both = vec![light, dark];
    both.sort();
    assert_eq!(block_refs, vec![both.clone()], "the landed block");
    assert_eq!(change_refs, vec![both.clone()], "the view's change");

    // one blob as both images, and a second figure on the same dark blob: each named once, and
    // a second upload of the same bytes stores nothing
    let end = r.store().log_end();
    assert_eq!(r.store_mut().put_blob(DARK).unwrap(), dark);
    assert_eq!(r.store().log_end(), end, "deduplicated");
    write(&mut r, "b", json!([figure_op(&s, &dark, Some(&dark))])).unwrap();
    let mut last = vec![];
    r.store()
        .for_each_frame_from(end, r.store().log_end(), |_, kind, _, payload, _| {
            if rdy_store::codec::logical_kind(kind) == kinds::CHANGE {
                last = refs_of(kind, payload).unwrap();
            }
            Ok(())
        })
        .unwrap();
    let named: Vec<Hash> = last
        .iter()
        .filter(|o| o.kind == Some(kinds::BLOB_MANIFEST))
        .map(|o| o.hash)
        .collect();
    assert_eq!(named, vec![dark]);

    // the tag reads both fields; compaction, reindex and fsck keep both images
    let at_tag = r.read().block(&Scope::At(landing), &fig).unwrap();
    assert_eq!(at_tag.field_str("blob"), Some(light.to_string().as_str()));
    assert_eq!(
        at_tag.field_str("dark_blob"),
        Some(dark.to_string().as_str())
    );
    r.compact(false).unwrap();
    r.reindex().unwrap();
    let report = r.fsck().unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(r.store().get_blob(&light).unwrap(), LIGHT);
    assert_eq!(r.store().get_blob(&dark).unwrap(), DARK);
    r.close().unwrap();
    rdy_qa::reindex_check::reindex_reproduces(d.path())
        .unwrap_or_else(|v| panic!("reindex: {v:#?}"));
    rdy_qa::invariants::check_all(&Rdy::open(d.path()).unwrap())
        .unwrap_or_else(|v| panic!("invariants: {v:#?}"));
}

// ---------------------------------------------------------------------------------------------
// the frozen fixture

/// The world of the fixture: a doc, a section and a paired figure, landed and tagged.
struct World {
    root: tempfile::TempDir,
    doc: String,
    figure: String,
    light: Hash,
    dark: Hash,
    landing: String,
    tag: String,
}

fn build_world() -> World {
    let (root, mut r) = rdy_qa::golden::tiny();
    let w = write(&mut r, "w0", doc_ops()).unwrap();
    let light = r.store_mut().put_blob(LIGHT).unwrap();
    let dark = r.store_mut().put_blob(DARK).unwrap();
    let s = ids(&w)["$s"].clone();
    let f = write(&mut r, "w0", json!([figure_op(&s, &light, Some(&dark))])).unwrap();
    let landed = land(&mut r, "w0").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.close().unwrap();
    World {
        root,
        doc: ids(&w)["$d"].clone(),
        figure: ids(&f)["$f"].clone(),
        light,
        dark,
        landing: landed,
        tag: "v1.0.0".into(),
    }
}

fn world_json(w: &World) -> Value {
    json!({
        "doc": w.doc,
        "figure": w.figure,
        "light": w.light.to_string(),
        "dark": w.dark.to_string(),
        "landing": w.landing,
        "tag": w.tag,
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

/// Writes the native store, its expect file, the dump and its expect file once, from one world.
/// Run by hand, never by the gate, after the readers fail: `cargo test -p rdy-qa --release
/// --test store rfr2_write_dark_fixtures_once -- --ignored`. It refuses a member that exists.
#[test]
#[ignore]
fn rfr2_write_dark_fixtures_once() {
    let dir = fixture_dir();
    let dump = qa_dir().join("fixtures/dump").join(DUMP);
    let dump_expect = qa_dir().join("fixtures/dump").join(DUMP_EXPECT);
    assert!(!dir.exists(), "figure-dark-1 is written once");
    for p in [&dump, &dump_expect] {
        assert!(!p.exists(), "{}: written once", p.display());
    }
    let w = build_world();
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
        "required_rdy": FIGURE_DARK_REQUIRED_RDY,
        "world": world_json(&w),
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
        "required_rdy": FIGURE_DARK_REQUIRED_RDY,
        "formats": rdy_qa::chain_fixture::current_formats(),
        "world": world_json(&w),
        "file": foreign_fixture::dump_file_facts(&dump),
        "whole": foreign_fixture::dump_load_facts(&dump, &scratch.path().join("loaded")),
    });
    let bytes = pretty(&v);
    write_new(&dump_expect, &bytes);
    eprintln!("{DUMP} blake3 {}", Hash::of(&dump_bytes));
    eprintln!("{DUMP_EXPECT} blake3 {}", Hash::of(&bytes));
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

/// The frozen native store opens as written: exactly the four store files and the expect file
/// with the pinned inventory; a copy with no index opens for writing, rebuilds its index, passes
/// fsck, reads every recorded fact (both blobs, the landing, the tag), keeps the manifest's
/// fixed minimum, survives compaction and a reindex, and the checked-in files never change.
#[test]
fn rfr2_dark_fixture_keeps_hashes_landings_and_tags() {
    let dir = fixture_dir();
    assert!(dir.join("expect.json").exists(), "figure-dark-1 is frozen");
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
    assert_eq!(want["published"], json!(false));
    assert_eq!(want["required_rdy"], json!(FIGURE_DARK_REQUIRED_RDY));
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
    // sentinel 19 and the manifest with figure_dark at its fixed minimum
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        rdy_store::format::format_file_text()
    );
    let dark_feature = want["store"]["manifest"]["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == FEATURE_FIGURE_DARK)
        .expect("the manifest holds figure_dark")
        .clone();
    assert_eq!(dark_feature["class"], "incompatible");
    assert_eq!(dark_feature["required_rdy"], FIGURE_DARK_REQUIRED_RDY);

    // the open of a copy reads back every recorded fact
    let (_base, root) = foreign_fixture::project_copy(&dir);
    let store = opened_facts(&root);
    assert!(root.join(".rdy/store/index.redb").exists(), "rebuilt");
    for member in want["store"].as_object().unwrap().keys() {
        assert_eq!(store[member], want["store"][member], "{member}");
    }
    let world = &want["world"];
    let light: Hash = world["light"].as_str().unwrap().parse().unwrap();
    let dark: Hash = world["dark"].as_str().unwrap().parse().unwrap();
    let fig = BlockId::parse(world["figure"].as_str().unwrap()).unwrap();
    let mut r = Rdy::open(&root).unwrap();
    assert_eq!(r.store().get_blob(&light).unwrap(), LIGHT);
    assert_eq!(r.store().get_blob(&dark).unwrap(), DARK);
    let landing: Hash = world["landing"].as_str().unwrap().parse().unwrap();
    for scope in [Scope::Canon, Scope::At(landing)] {
        let b = r.read().block(&scope, &fig).unwrap();
        assert_eq!(b.field_str("dark_blob"), Some(dark.to_string().as_str()));
    }
    assert!(
        store["landings"]
            .to_string()
            .contains(world["landing"].as_str().unwrap()),
        "the landing"
    );
    // compaction and a reindex keep both images and the recorded identities
    r.compact(false).unwrap();
    r.reindex().unwrap();
    assert_eq!(r.store().get_blob(&dark).unwrap(), DARK);
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
