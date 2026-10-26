//! ok13: the first native store fixture (D-XTWM9: "Store fixtures start at Release 1"; D-WWAK7:
//! each published fixture opens and passes fsck; D-69AY3). The fixture `feature-1` holds the
//! store files of the project the shared every-kind builders write at object encoding 1
//! (`chain_fixture::project_chain_current`): the manifest of grammar 1 with all seven registered
//! features, its segments and the regular lock file. It holds no derived index: an open rebuilds
//! it. The wire fixtures of the dump and sync formats stay as they are; this one covers the
//! native segment open that no dump can test.
//!
//! - `FEATURES`, `FORMAT`, `lock` and every file of `segments/` are the store files, byte for
//!   byte. The
//!   builder rolls its source log every 4096 bytes and the released chain it extends keeps its
//!   own segment places (a payload names its places), so the log is many sealed segments
//!   (`seg-000000.log` to the last one), all far below the 64 MiB roll threshold.
//! - `expect.json` records what an open must read from them and the relative path, length and
//!   blake3 of each store file. It names no absolute path and no host. `published` is false until
//!   Release 1 ships and changes once, before publication; after that every file is immutable.
//!
//! Write the fixture once: never rewrite it, and never regenerate it to fix a later reader. The
//! tests copy the store files into a fresh project and never open the checked-in files.
use rdy_engine::{Rdy, Scope, read_only_feature_for_tests};
use rdy_model::Hash;
use rdy_qa::chain_fixture;
use rdy_qa::every_kind;
use rdy_store::format::StoreAccess;
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The blake3 of the inventory text (`inventory_text`): one line `<path> <len> <blake3>` for the
/// store files `FEATURES`, `FORMAT`, `lock` and each segment, in path order. Set when the fixture was written.
/// A change here needs a founder decision, and after Release 1 it is never allowed.
const INVENTORY_BLAKE3: &str = "5958aa4e476d53d7c95cb960896e2be5e29547340237ccb2199caa90d3e62e53";

/// The number of segment files of the fixture, set when it was written.
const SEGMENT_COUNT: usize = 61;

/// The members of the expect file's `store` object that record the code at write time and not a
/// fact an open reads from the store. The fixture is immutable, so the reader must not compare
/// them with the current code: a new latest encoding would fail a store that still opens.
const CODE_RECORDED: [&str; 1] = ["encodings"];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/store/feature-1")
}

fn expect_path() -> PathBuf {
    fixture_dir().join("expect.json")
}

fn read_expect() -> Value {
    serde_json::from_str(&std::fs::read_to_string(expect_path()).unwrap()).unwrap()
}

fn digest_of(p: &Path) -> (u64, String) {
    let bytes = std::fs::read(p).unwrap();
    (bytes.len() as u64, Hash::of(&bytes).to_string())
}

/// The relative path of every file under `dir`, sorted: the store files, and `expect.json` when
/// `dir` is the fixture.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if std::fs::symlink_metadata(&p).unwrap().is_dir() {
                walk(&p, base, out);
            } else {
                out.push(
                    p.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut out = vec![];
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// The store files under `dir`: every file but `expect.json` and the derived index.
fn store_files(dir: &Path) -> Vec<String> {
    files_under(dir)
        .into_iter()
        .filter(|f| f != "expect.json" && !f.starts_with("index.redb"))
        .collect()
}

/// The length and blake3 of each fixture file, by relative path.
fn source_digests() -> BTreeMap<String, (u64, String)> {
    files_under(&fixture_dir())
        .into_iter()
        .map(|f| {
            let d = digest_of(&fixture_dir().join(&f));
            (f, d)
        })
        .collect()
}

/// The inventory text of the store files under `dir`.
fn inventory_text(dir: &Path) -> String {
    store_files(dir)
        .iter()
        .map(|f| {
            let (len, blake3) = digest_of(&dir.join(f));
            format!("{f} {len} {blake3}\n")
        })
        .collect()
}

fn pretty(v: &Value) -> Vec<u8> {
    (serde_json::to_string_pretty(v).unwrap() + "\n").into_bytes()
}

/// Every path under `dir` with its contents (None for a directory).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let m = std::fs::symlink_metadata(&p).unwrap();
        if m.is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some(std::fs::read(&p).unwrap()));
        }
    }
    out
}

/// A fresh temporary project whose `.rdy/store/` holds copies of the store files in
/// `from`, with the QA pack the builders use. No index exists.
fn project_copy(from: &Path) -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    let store = root.join(".rdy/store");
    std::fs::create_dir_all(store.join("segments")).unwrap();
    std::fs::create_dir_all(root.join(".rdy/packs")).unwrap();
    let files = store_files(from);
    std::fs::write(root.join(".rdy.yaml"), "pack: rdy-qa\n").unwrap();
    std::fs::write(
        root.join(".rdy/packs/rdy-qa.yaml"),
        rdy_qa::packs::QA_PACK_YAML,
    )
    .unwrap();
    for f in &files {
        std::fs::copy(from.join(f), store.join(f)).unwrap();
    }
    assert!(!store.join("index.redb").exists(), "no index before open");
    (base, root)
}

fn origin() -> Location {
    Location {
        segment: 0,
        offset: 0,
    }
}

/// What an open of the store must read, as the expect file records it: the manifest, the
/// frame and batch counts, the object keys, the refs, the tags, the landings, the raw encodings
/// and the signatures. It also writes `encodings`, the latest-encoding map of the code that wrote
/// the fixture. That member is a record of the writer, not a fact of the store: the reader never
/// compares it (`CODE_RECORDED`), and the io written-format guard holds it.
fn store_facts(r: &Rdy) -> Value {
    let end = r.store().log_end();
    let (mut frames, mut batches) = (0u64, 0u64);
    r.read()
        .reader()
        .for_each_record_from(origin(), end, |_, f, _| {
            match f {
                StoredFrame::Present { kind, .. } if kind == kinds::BATCH_COMMIT => batches += 1,
                _ => frames += 1,
            }
            Ok(())
        })
        .unwrap();
    let facts = every_kind::facts(r);
    let identity = every_kind::identity_facts(r, Some(end));
    let features = every_kind::feature_facts(r, end);
    json!({
        "manifest": features["manifest"],
        "encodings": chain_fixture::current_formats()["encodings"],
        "raw_encodings": features["encodings"],
        "log_end": [end.segment, end.offset],
        "frames": frames,
        "batches": batches,
        "objects": facts["objects"],
        "object_keys": identity["keys"],
        "redacted": identity["redacted"],
        "refs": facts["refs"],
        "tags": identity["tags"],
        "landings": facts["landings"],
        "signatures": facts["signatures"],
        "fsck": without_moves(&facts["fsck"]),
    })
}

/// The fsck report without its `unlogged_moves` count: analysis a later build changes, not damage.
fn without_moves(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove("unlogged_moves");
    }
    v
}

fn inventory(dir: &Path) -> Value {
    Value::Array(
        store_files(dir)
            .iter()
            .map(|f| {
                let (len, blake3) = digest_of(&dir.join(f));
                json!({"path": f, "len": len, "blake3": blake3})
            })
            .collect(),
    )
}

/// The producer commit: the commit the code that wrote the fixture is at.
fn producer_commit() -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(out.status.success(), "git rev-parse HEAD");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Writes the fixture once, from the shared builder. Run by hand, never by the gate, after the
/// readers fail: `cargo test -p rdy-qa --release --test store ok13_write_native_feature_fixture_once
/// -- --ignored`. It refuses a fixture that exists.
#[test]
#[ignore]
fn ok13_write_native_feature_fixture_once() {
    let dir = fixture_dir();
    assert!(
        !dir.exists(),
        "the native feature-1 fixture is written once: never rewrite it"
    );
    // the store of the builder's project, closed: its files are copied byte for byte
    let chain = chain_fixture::project_chain_current();
    let store = chain.root.join(".rdy/store");
    let mut copied = vec![];
    for f in store_files(&store) {
        let to = dir.join(&f);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&to)
            .unwrap();
        std::io::Write::write_all(&mut out, &std::fs::read(store.join(&f)).unwrap()).unwrap();
        out.sync_all().unwrap();
        copied.push(f);
    }
    assert_eq!(copied[..3], ["FEATURES", "FORMAT", "lock"]);
    for f in &copied[3..] {
        assert!(
            f.starts_with("segments/seg-") && f.ends_with(".log"),
            "{f}: a store file"
        );
    }
    let segments = copied.len() - 3;
    eprintln!("segments {segments}");
    // the facts are read from a copy of the files just written, as the readers read them
    let (_base, root) = project_copy(&dir);
    let mut r = Rdy::open(&root).unwrap();
    r.reindex().unwrap();
    let mut expect = json!({
        "published": false,
        "producer_commit": producer_commit(),
        "inventory": inventory(&dir),
    });
    expect["store"] = store_facts(&r);
    r.close().unwrap();
    let bytes = pretty(&expect);
    std::fs::write(expect_path(), &bytes).unwrap();
    eprintln!(
        "INVENTORY_BLAKE3 {}",
        Hash::of(inventory_text(&dir).as_bytes())
    );
    eprintln!("expect.json blake3 {}", Hash::of(&bytes));
}

/// Opens a copy of the fixture's store files with Rdy: the index is absent, so the open and the
/// reindex rebuild it. Runs full fsck and compares every recorded fact.
fn open_checked_copy() {
    let want = read_expect();
    let (_base, root) = project_copy(&fixture_dir());
    let mut r = Rdy::open(&root).unwrap();
    assert_eq!(r.store().access(), StoreAccess::ReadWrite);
    r.reindex().unwrap();
    assert!(root.join(".rdy/store/index.redb").exists(), "rebuilt");
    // the full fsck: tag chains, signatures, transition folds and store objects
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.foreign_signatures, 0);
    let got = store_facts(&r);
    assert_eq!(got["fsck"], want["store"]["fsck"], "fsck report");
    for member in want["store"].as_object().unwrap().keys() {
        if CODE_RECORDED.contains(&member.as_str()) {
            continue;
        }
        assert_eq!(got[member], want["store"][member], "{member}");
    }
    // the carriers read back: canon materializes and the invariants hold
    let canon = r.read().materialized(&Scope::Canon).unwrap();
    assert!(canon.iter().count() > 0);
    r.close().unwrap();
    rdy_qa::invariants::check_all(&Rdy::open(&root).unwrap())
        .unwrap_or_else(|v| panic!("invariants: {v:#?}"));
    rdy_qa::reindex_check::reindex_reproduces(&root).unwrap_or_else(|v| panic!("reindex: {v:#?}"));
}

/// A copy that uses a read-only feature (the test hook of D-WWAK7) opens for reading and changes
/// no byte of the project: the regular lock file included.
fn open_read_only_copy() {
    let (_base, root) = project_copy(&fixture_dir());
    let lock = root.join(".rdy/store/lock");
    assert!(
        std::fs::symlink_metadata(&lock).unwrap().is_file(),
        "the fixture keeps its regular lock file"
    );
    {
        let _known = read_only_feature_for_tests();
        let mut r = Rdy::open(&root).unwrap();
        r.store_mut()
            .put_blob(b"uses the read-only feature")
            .unwrap();
        r.seal_open().unwrap();
        r.close().unwrap();
    }
    // the next open knows no such feature; no index exists
    std::fs::remove_file(root.join(".rdy/store/index.redb")).unwrap();
    let before = tree(&root);
    assert!(before.contains_key(&lock), "the lock is preserved");
    let r = Rdy::open(&root).unwrap();
    assert_eq!(r.store().access(), StoreAccess::ReadOnly);
    assert!(r.is_read_only());
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(r.read().materialized(&Scope::Canon).unwrap().iter().count() > 0);
    r.close().unwrap();
    assert_eq!(tree(&root), before, "no source-store path or byte changed");
    assert!(std::fs::symlink_metadata(&lock).unwrap().is_file());
}

#[test]
fn ok13_native_feature_store_rebuilds_and_checks() {
    // the opened store reports the recorded manifest (compared member by member inside)
    open_checked_copy();
    // the recorded manifest is grammar 1 with the seven features registered when it was written,
    // each through first use. These are facts of the frozen fixture, not of the current code: a
    // later feature changes the code's registry and leaves this store as it is.
    let want = read_expect();
    let manifest = &want["store"]["manifest"];
    assert_eq!(manifest["manifest_version"], 1);
    let features = manifest["features"].as_array().unwrap();
    assert_eq!(features.len(), 7, "{manifest}");
    let names: Vec<&str> = features
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "sorted by name");
    sorted.dedup();
    assert_eq!(sorted.len(), 7, "no feature twice");
}

#[test]
fn ok13_native_fixture_has_a_read_only_lock() {
    open_read_only_copy();
}

#[test]
fn ok13_fixture_copy_never_changes_source() {
    let before = source_digests();
    open_checked_copy();
    assert_eq!(source_digests(), before, "after the writable open");
    open_read_only_copy();
    assert_eq!(source_digests(), before, "after the read-only open");
    // the store files are pinned apart from the expect file, which changes once at Release 1
    let dir = fixture_dir();
    assert_eq!(
        Hash::of(inventory_text(&dir).as_bytes()).to_string(),
        INVENTORY_BLAKE3
    );
    // the directory holds the store files and expect.json and nothing else: no derived index
    let store = store_files(&dir);
    assert_eq!(store.len(), SEGMENT_COUNT + 3);
    assert_eq!(store[..3], ["FEATURES", "FORMAT", "lock"]);
    for (i, f) in store[3..].iter().enumerate() {
        assert_eq!(f, &format!("segments/seg-{i:06}.log"), "no gap");
    }
    let mut all = store.clone();
    all.push("expect.json".into());
    all.sort();
    assert_eq!(files_under(&dir), all);
}

#[test]
fn ok13_store_fixture_publication_marker_is_explicit() {
    let want = read_expect();
    assert!(want["published"].is_boolean(), "{}", want["published"]);
    let commit = want["producer_commit"].as_str().unwrap();
    assert!(
        commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "{commit}"
    );
    // the inventory entries are exact: FEATURES, FORMAT, every segment and the lock
    let items = want["inventory"].as_array().unwrap();
    let files = store_files(&fixture_dir());
    assert_eq!(items.len(), files.len());
    assert_eq!(items.len(), SEGMENT_COUNT + 3);
    for (item, f) in items.iter().zip(&files) {
        let (len, blake3) = digest_of(&fixture_dir().join(f));
        assert_eq!(
            item,
            &json!({"path": f, "len": len, "blake3": blake3}),
            "{f}"
        );
    }
    assert_eq!(
        Hash::of(inventory_text(&fixture_dir()).as_bytes()).to_string(),
        INVENTORY_BLAKE3
    );
    // no absolute path, and no value that is a path other than the inventory's relative ones
    let text = std::fs::read_to_string(expect_path()).unwrap();
    for bad in ["/Users", "/home/", "/tmp", "/var/", "\\\\", ":\\"] {
        assert!(!text.contains(bad), "the expect file names {bad}");
    }
    // no host: the file holds only its four members, none of them a host's, and no text names
    // this machine or a local network name
    let mut keys: Vec<&str> = want
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, ["inventory", "producer_commit", "published", "store"]);
    let host = std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_lowercase())
        .unwrap_or_default();
    let lower = text.to_lowercase();
    for name in [host.as_str(), host.split('.').next().unwrap_or("")] {
        assert!(
            name.len() < 3 || !lower.contains(name),
            "the expect file names this host {name}"
        );
    }
    for bad in [".local\"", ".lan\"", ".internal\"", "localhost"] {
        assert!(!lower.contains(bad), "the expect file names a host: {bad}");
    }
}

/// Final ruling 3 (round 2), end to end: a released rdy (`RDY_RELEASED_RDY`, the path of its
/// binary; rdy 0.24.0 when this pin was added) opening an M3a store, the feature-1 fixture's and
/// a new one, says a newer rdy wrote it, never that FORMAT is unreadable, and changes no byte.
/// It needs a released binary, so the gate skips it; run it by hand: `RDY_RELEASED_RDY=<path>
/// cargo test -p rdy-qa --release --test store ok13_a_released_rdy -- --ignored`.
#[test]
#[ignore = "needs RDY_RELEASED_RDY, the path of a released rdy binary"]
fn ok13_a_released_rdy_reads_an_m3a_store_as_newer() {
    let bin = std::env::var_os("RDY_RELEASED_RDY").expect("RDY_RELEASED_RDY names a released rdy");
    let version = std::process::Command::new(&bin)
        .arg("--version")
        .output()
        .unwrap();
    eprintln!(
        "released rdy: {}",
        String::from_utf8_lossy(&version.stdout).trim()
    );
    let (_fixture, copied) = project_copy(&fixture_dir());
    let fresh = tempfile::tempdir().unwrap();
    drop(Rdy::init(fresh.path(), "rdy-standard").unwrap());
    for root in [copied.as_path(), fresh.path()] {
        let before = tree(&root.join(".rdy"));
        let out = std::process::Command::new(&bin)
            .args(["status", "--no-daemon"])
            .current_dir(root)
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        eprintln!("{}: {text}", root.display());
        assert!(!out.status.success(), "{text}");
        assert!(text.contains("a newer rdy wrote this store"), "{text}");
        assert!(!text.contains("unreadable"), "{text}");
        assert_eq!(tree(&root.join(".rdy")), before, "no byte changed");
    }
}
