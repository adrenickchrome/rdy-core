//! rpl33 (repo links; D-FK58A, D-WWAK7, D-69AY3): the frozen first-use `foreign_copy` fixtures.
//! One world (`rdy_qa::foreign_fixture`) gives three members, each written once and never
//! rewritten:
//!
//! - the native store `fixtures/store/foreign-copy-1/`: `FEATURES`, `FORMAT`, `lock` and one
//!   segment, byte for byte, and no derived index. A dump cannot prove that a native store
//!   opens; this store does. Its `expect.json` records what an open reads (the store facts, as
//!   `feature-1` records them, and the foreign copy), the inventory, the producer commit and the
//!   minimum rdy of `foreign_copy`. `published` is false until the release that carries
//!   `foreign_copy` ships, and changes once, before publication.
//! - the dump `fixtures/dump/v4-f1-e1-foreign-copy.rdydump` of the same project, with its expect
//!   file (`release_repo_links_formats` reads it).
//! - the link delivery `fixtures/sync/v3-f1-e1-link-delivery.rdysync` (sync format 3, purpose
//!   `link_delivery`) that the cited project wrote at its first tag, with its expect file
//!   (`release_repo_links_formats` and rdy-store's `foreign_compatibility` read it).
//!
//! The project holds both repo link sources (one live, one revoked), the deliveries, foreign
//! pins and a foreign `decided_by` link, a person's adoption of a changed cited block, and a
//! cited removal whose `Removal` entry erased the citing copy.
use rdy_engine::Rdy;
use rdy_model::Hash;
use rdy_qa::foreign_fixture::{self, ForeignWorld};
use rdy_store::format::{FOREIGN_COPY_REQUIRED_RDY, StoreAccess};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The blake3 of the inventory text of the native store files, set when they were written. A
/// change here needs a founder decision, and after publication it is never allowed.
const INVENTORY_BLAKE3: &str = "4952d7773ad10b100d1d268feff55a74ad54fc6f66ca8924380c2da022031f23";

/// The members' file names.
const DUMP: &str = "v4-f1-e1-foreign-copy.rdydump";
const DUMP_EXPECT: &str = "v4-f1-e1-foreign-copy.expect.json";
const SYNC: &str = "v3-f1-e1-link-delivery.rdysync";
const SYNC_EXPECT: &str = "v3-f1-e1-link-delivery.expect.json";

fn qa_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fixture_dir() -> PathBuf {
    qa_dir().join("fixtures/store/foreign-copy-1")
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn pretty(v: &Value) -> Vec<u8> {
    (serde_json::to_string_pretty(v).unwrap() + "\n").into_bytes()
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

/// The open facts of the project at `root`: rebuilt index, full fsck, store and foreign facts.
fn opened_facts(root: &Path) -> (Value, Value) {
    let mut r = Rdy::open(root).unwrap();
    assert_eq!(r.store().access(), StoreAccess::ReadWrite);
    r.reindex().unwrap();
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let facts = (
        foreign_fixture::store_facts(&r),
        foreign_fixture::foreign_facts(&r),
    );
    r.close().unwrap();
    facts
}

/// The ids and records of the world, as every expect file names them.
fn world_json(w: &ForeignWorld) -> Value {
    json!({
        "repo": w.repo.to_string(),
        "kept": w.kept.to_string(),
        "removed": w.removed.to_string(),
        "decision": w.decision.to_string(),
        "live": w.live.to_string(),
        "revoked": w.revoked.to_string(),
        "claim": w.claim.to_string(),
        "adopt_answer": w.adopt_answer.to_string(),
        "removal_entry": w.removal_entry.to_string(),
    })
}

/// The native store's expect file, from the store files now in `dir`.
fn store_expect(dir: &Path, w: &ForeignWorld) -> Value {
    let (_base, root) = foreign_fixture::project_copy(dir);
    let (store, foreign) = opened_facts(&root);
    json!({
        "published": false,
        "producer_commit": producer_commit(),
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": FOREIGN_COPY_REQUIRED_RDY,
        "world": world_json(w),
        "inventory": foreign_fixture::inventory(dir),
        "store": store,
        "foreign": foreign,
    })
}

/// Writes the three members once, from one world. Run by hand, never by the gate, after the
/// readers fail: `cargo test -p rdy-qa --release --test store rpl33_write_foreign_fixtures_once
/// -- --ignored`. It refuses a member that exists.
#[test]
#[ignore]
fn rpl33_write_foreign_fixtures_once() {
    let dir = fixture_dir();
    let dump = qa_dir().join("fixtures/dump").join(DUMP);
    let dump_expect = qa_dir().join("fixtures/dump").join(DUMP_EXPECT);
    let sync = qa_dir().join("fixtures/sync").join(SYNC);
    let sync_expect = qa_dir().join("fixtures/sync").join(SYNC_EXPECT);
    assert!(!dir.exists(), "foreign-copy-1 is written once");
    for p in [&dump, &dump_expect, &sync, &sync_expect] {
        assert!(!p.exists(), "{}: written once", p.display());
    }
    let w = foreign_fixture::build();
    // the native store files, byte for byte, with no index
    let store = w.citing_root.join(".rdy/store");
    let copied = foreign_fixture::store_files(&store);
    assert_eq!(
        copied,
        ["FEATURES", "FORMAT", "lock", "segments/seg-000000.log"],
        "one segment"
    );
    for f in &copied {
        write_new(&dir.join(f), &std::fs::read(store.join(f)).unwrap());
    }
    let expect = store_expect(&dir, &w);
    let bytes = pretty(&expect);
    write_new(&dir.join("expect.json"), &bytes);
    eprintln!(
        "INVENTORY_BLAKE3 {}",
        Hash::of(foreign_fixture::inventory_text(&dir).as_bytes())
    );
    eprintln!("expect.json blake3 {}", Hash::of(&bytes));

    // the dump of the same project, and its expect file read from the written member
    let scratch = tempfile::tempdir().unwrap();
    let dumped = scratch.path().join("p.rdydump");
    let r = Rdy::open(&w.citing_root).unwrap();
    r.read().dump(&dumped).unwrap();
    r.close().unwrap();
    let dump_bytes = std::fs::read(&dumped).unwrap();
    write_new(&dump, &dump_bytes);
    let v = json!({
        "published": false,
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": FOREIGN_COPY_REQUIRED_RDY,
        "formats": rdy_qa::chain_fixture::current_formats(),
        "world": world_json(&w),
        "file": foreign_fixture::dump_file_facts(&dump),
        "whole": foreign_fixture::dump_load_facts(&dump, &scratch.path().join("loaded")),
    });
    let bytes = pretty(&v);
    write_new(&dump_expect, &bytes);
    eprintln!("{DUMP} blake3 {}", Hash::of(&dump_bytes));
    eprintln!("{DUMP_EXPECT} blake3 {}", Hash::of(&bytes));

    // the first delivery, as the cited project wrote it
    write_new(&sync, &w.delivery);
    let mut v = foreign_fixture::delivery_facts(&w.delivery);
    v["published"] = json!(false);
    v["rdy"] = json!(env!("CARGO_PKG_VERSION"));
    v["world"] = world_json(&w);
    v["taken"] = foreign_fixture::delivery_take_facts(&w.delivery);
    let bytes = pretty(&v);
    write_new(&sync_expect, &bytes);
    eprintln!("{SYNC} blake3 {}", Hash::of(&w.delivery));
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

/// rpl33: the frozen native store opens as written. Its files are exactly the four store files
/// and the expect file, with the pinned inventory; a copy of them with no index opens for
/// writing, rebuilds its index, passes the full fsck with no warning and reads every recorded
/// fact; the foreign copy holds what the fixture promises; the invariants hold and a reindex
/// reproduces the store; the checked-in files never change.
#[test]
fn rpl33_frozen_native_foreign_store_rebuilds_and_passes_fsck() {
    let dir = fixture_dir();
    assert!(
        dir.join("expect.json").exists(),
        "the native foreign-copy-1 fixture is frozen in this task"
    );
    let before = source_digests();
    let want = read_json(&dir.join("expect.json"));

    // the files: four store files, one segment, no index; the pinned inventory
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

    // sentinel 19 and the manifest with foreign_copy and its minimum rdy, as written
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        rdy_store::format::format_file_text()
    );
    let manifest = &want["store"]["manifest"];
    let foreign = manifest["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "foreign_copy")
        .expect("the manifest holds foreign_copy");
    assert_eq!(foreign["class"], "incompatible");
    assert_eq!(foreign["required_rdy"], want["required_rdy"]);
    let features_text = std::fs::read_to_string(dir.join("FEATURES")).unwrap();
    assert!(
        features_text.contains("\"name\":\"foreign_copy\""),
        "{features_text}"
    );

    // the open of a copy: the index is rebuilt, fsck passes, every recorded fact reads back
    let (_base, root) = foreign_fixture::project_copy(&dir);
    let (store, foreign) = opened_facts(&root);
    assert!(root.join(".rdy/store/index.redb").exists(), "rebuilt");
    for member in want["store"].as_object().unwrap().keys() {
        assert_eq!(store[member], want["store"][member], "{member}");
    }
    assert_eq!(foreign, want["foreign"], "the foreign copy");

    // what the fixture promises: both sources (one revoked), deliveries, a revocation and a
    // removal, pins and a foreign link on the citing claim, an adoption, a removed version
    let world = &want["world"];
    assert_eq!(
        foreign_fixture::event_kinds(&foreign),
        ["delivery", "delivery", "delivery", "removal", "revoked"]
    );
    let sources = foreign["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    let revoked: Vec<bool> = sources
        .iter()
        .map(|s| s["revoked"].as_bool().unwrap())
        .collect();
    assert!(
        revoked.contains(&true) && revoked.contains(&false),
        "{sources:?}"
    );
    let claim = &foreign["parts"][world["claim"].as_str().unwrap()];
    assert_eq!(claim["pins"].as_object().unwrap().len(), 3, "{claim}");
    assert_eq!(claim["links"]["decided_by"].as_array().unwrap().len(), 1);
    assert_eq!(
        foreign["adopt_answers"][0]["hash"], world["adopt_answer"],
        "the person's adoption"
    );
    assert_eq!(foreign["removed_blocks"].as_array().unwrap().len(), 1);
    let entries = foreign["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|e| e["hash"] == world["removal_entry"]),
        "the Removal entry"
    );

    // the invariants hold and a reindex reproduces the store
    rdy_qa::invariants::check_all(&Rdy::open(&root).unwrap())
        .unwrap_or_else(|v| panic!("invariants: {v:#?}"));
    rdy_qa::reindex_check::reindex_reproduces(&root).unwrap_or_else(|v| panic!("reindex: {v:#?}"));

    // the checked-in files never change
    assert_eq!(source_digests(), before);
}
