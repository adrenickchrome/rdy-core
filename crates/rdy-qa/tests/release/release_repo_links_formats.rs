//! rpl33 (repo links; D-69AY3, D-WWAK7, D-TXFHR, D-E30RA): the released-format compatibility
//! matrix after `foreign_copy`.
//!
//! - Every released dump, and the frozen first-use `foreign_copy` dump, loads into this rdy and
//!   passes fsck; every released dump and sync fixture keeps the length and digest that the
//!   Release 1 contract (`contracts/release-1.json`, frozen at the Release 1 tag) records.
//! - The published Release 1 store fixture (`store/feature-1`) opens in place: a dry-run upgrade
//!   plans nothing, the open adds no feature, appends nothing and keeps every object key,
//!   landing, tag and signature (no rekey).
//! - An old rdy refuses the new feature and the new stream. The facts come from what Release 1
//!   reads, as its frozen contract records it, and from the frozen members' own bytes: never
//!   from a newer decoder standing in for the old one. The run of an actual released binary is
//!   [`rpl33_a_released_rdy_refuses_foreign_feature_and_stream`]: it needs `RDY_RELEASED_RDY`.
use rdy_engine::Rdy;
use rdy_engine::load::{load, load_chain};
use rdy_model::Hash;
use rdy_qa::foreign_fixture;
use rdy_store::dump::{DUMP_FORMAT, READS, READS_FEATURES, read_envelope};
use rdy_store::format::{DumpStore, FOREIGN_COPY_REQUIRED_RDY, StoreAccess};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The rdy of Release 1 (tag `v0.47.0`), whose frozen contract this file reads.
const RELEASE_ONE_RDY: &str = "0.47.0";
const DUMP: &str = "v4-f1-e1-foreign-copy.rdydump";
const DUMP_EXPECT: &str = "v4-f1-e1-foreign-copy.expect.json";
const SYNC: &str = "v3-f1-e1-link-delivery.rdysync";
const SYNC_EXPECT: &str = "v3-f1-e1-link-delivery.expect.json";

/// The new members and their blake3, set when they were written. A change needs a founder
/// decision; after publication none is allowed.
const MEMBERS: [(&str, &str, &str); 4] = [
    (
        "dump",
        DUMP,
        "6aca7c40fefdff88db530fdbce96355f38e068c85e65ba4e8e627b9977a78bbf",
    ),
    (
        "dump",
        DUMP_EXPECT,
        "1d74a9af451d64935717c941b16fb656888f740ff655bb6a681ef00b0b72afd3",
    ),
    (
        "sync",
        SYNC,
        "b38779eba592db56274f4a156edb70baacbe4a51dbfe9ca70242e65586eb483f",
    ),
    (
        "sync",
        SYNC_EXPECT,
        "c42da926057a2f54c614520103b32592c0f3ac213d8dcfe53cbdfb4b05f250ae",
    ),
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The Release 1 contract, frozen at the Release 1 tag.
fn release_one() -> Value {
    read_json(&fixtures().join("contracts/release-1.json"))
}

/// The `(major, minor, patch)` of a plain version.
fn version_key(v: &str) -> (u64, u64, u64) {
    let p: Vec<u64> = v.split('.').map(|x| x.parse().unwrap()).collect();
    assert_eq!(p.len(), 3, "{v}");
    (p[0], p[1], p[2])
}

/// Every file of `dir` the Release 1 contract's `digests` row names keeps its length and blake3.
fn released_digests_hold(dir: &Path, digests: &Value) -> usize {
    let digests = digests.as_object().unwrap();
    for (name, recorded) in digests {
        let bytes = std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            &format!("{} {}", bytes.len(), Hash::of(&bytes)),
            recorded.as_str().unwrap(),
            "{name}: a released fixture never changes"
        );
    }
    digests.len()
}

/// One dump fixture to load: its name, whether it is a holder's, and its files (a chain's full
/// dump and its increments, in order).
struct Input {
    name: String,
    holder: bool,
    files: Vec<PathBuf>,
}

/// Every dump fixture under `fixtures/dump`, a chain as one input.
fn dump_inputs() -> Vec<Input> {
    let dir = fixtures().join("dump");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".rdydump"))
        .collect();
    names.sort();
    let mut out = vec![];
    for n in &names {
        let stem = n.trim_end_matches(".rdydump");
        let holder = stem.contains("holder");
        if let Some(prefix) = stem.strip_suffix("-chain-full") {
            out.push(Input {
                name: format!("{prefix}-chain"),
                holder,
                files: ["full", "1", "2"]
                    .iter()
                    .map(|p| dir.join(format!("{prefix}-chain-{p}.rdydump")))
                    .collect(),
            });
        } else if !(stem.ends_with("-chain-1") || stem.ends_with("-chain-2")) {
            out.push(Input {
                name: stem.to_string(),
                holder,
                files: vec![dir.join(n)],
            });
        }
    }
    out
}

/// rpl33: every released dump and the frozen `foreign_copy` dump load into this rdy. Each
/// released dump and sync fixture keeps the bytes Release 1 recorded; each dump's formats are
/// ones this rdy reads; each loads (a chain whole) and passes fsck as a project or a holder. The
/// `foreign_copy` dump keeps its pinned bytes, and its file and whole-load facts are the ones
/// its expect file records: the same store as the frozen native store.
#[test]
fn rpl33_every_released_dump_loads() {
    let r1 = release_one();
    let dumps = released_digests_hold(
        &fixtures().join("dump"),
        &r1["dump_load"]["fixture_digests"],
    );
    let syncs = released_digests_hold(&fixtures().join("sync"), &r1["sync"]["fixture_digests"]);
    assert!(dumps >= 70 && syncs >= 8, "{dumps} {syncs}");

    // the new members are frozen
    for (area, name, digest) in MEMBERS {
        let p = fixtures().join(area).join(name);
        assert!(
            p.exists(),
            "{name}: the foreign_copy members are frozen in this task"
        );
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), digest, "{name}");
    }

    // every dump fixture: formats this rdy reads, a load, fsck
    let inputs = dump_inputs();
    let scratch = tempfile::tempdir().unwrap();
    let (mut loaded, mut holders, mut foreign) = (0, 0, 0);
    for (i, input) in inputs.iter().enumerate() {
        let facts = rdy_engine::formats::inspect_dump(&input.files[0])
            .unwrap_or_else(|e| panic!("{}: {e:?}", input.name));
        match &facts.store {
            DumpStore::Legacy(n) => assert!(READS.contains(&(facts.dump, *n)), "{}", input.name),
            DumpStore::Features(m) => {
                assert_eq!(facts.dump, DUMP_FORMAT, "{}", input.name);
                assert!(
                    READS_FEATURES.contains(&m.manifest_version),
                    "{}",
                    input.name
                );
                foreign += usize::from(m.features.iter().any(|f| f.name == "foreign_copy"));
            }
        }
        let to = scratch.path().join(format!("l{i}"));
        let done = if input.files.len() == 1 {
            load(&input.files[0], &to)
        } else {
            load_chain(&input.files, &to, None)
        };
        done.unwrap_or_else(|e| panic!("{}: {e:?}", input.name));
        let r = if input.holder {
            Rdy::open_holder(&to).unwrap()
        } else {
            Rdy::open(&to).unwrap()
        };
        r.fsck()
            .unwrap_or_else(|e| panic!("{}: fsck refused the load: {e:?}", input.name));
        r.close().unwrap();
        loaded += 1;
        holders += usize::from(input.holder);
    }
    assert!(loaded >= 30 && holders >= 10, "{loaded} {holders}");
    assert_eq!(foreign, 1, "one dump uses foreign_copy");

    // the foreign_copy dump: its file, its whole load, and the frozen native store's facts
    let file = fixtures().join("dump").join(DUMP);
    let expect = read_json(&fixtures().join("dump").join(DUMP_EXPECT));
    assert_eq!(foreign_fixture::dump_file_facts(&file), expect["file"]);
    assert_eq!(expect["file"]["dump_format"], json!(4));
    let manifest = &expect["file"]["store_format"]["features"]["features"];
    let fc = manifest
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "foreign_copy")
        .expect("the dump's manifest holds foreign_copy");
    assert_eq!(fc["class"], "incompatible");
    assert_eq!(fc["required_rdy"], expect["required_rdy"]);
    let whole = foreign_fixture::dump_load_facts(&file, &scratch.path().join("foreign"));
    assert_eq!(whole, expect["whole"], "the whole load");
    let native = read_json(&fixtures().join("store/foreign-copy-1/expect.json"));
    assert_eq!(
        whole["store"], native["store"],
        "the dump holds the native store"
    );
    assert_eq!(whole["foreign"], native["foreign"]);
}

/// rpl33: the frozen link delivery (sync stream format 3) reads to the frames, delivery,
/// selection identity and delivery digest its expect file records, and a new project with a
/// repo link to its repo takes it as recorded: every selected block taken and held, and a second
/// take of the same file unchanged.
#[test]
fn rpl33_frozen_delivery_takes_as_recorded() {
    let bytes = std::fs::read(fixtures().join("sync").join(SYNC)).unwrap();
    let expect = read_json(&fixtures().join("sync").join(SYNC_EXPECT));
    let facts = foreign_fixture::delivery_facts(&bytes);
    for member in ["stream", "delivery", "selection", "same_delivery_digest"] {
        assert_eq!(facts[member], expect[member], "{member}");
    }
    let taken = foreign_fixture::delivery_take_facts(&bytes);
    assert_eq!(taken, expect["taken"]);
    assert_eq!(taken["selection"], expect["selection"]);
    assert_eq!(taken["again_unchanged"], json!(true));
}

/// A new project whose store is a copy of the published Release 1 store fixture, with the QA
/// pack its builders used. No index exists.
fn release_one_copy() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let from = fixtures().join("store/feature-1");
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    let store = root.join(".rdy/store");
    std::fs::create_dir_all(store.join("segments")).unwrap();
    std::fs::create_dir_all(root.join(".rdy/packs")).unwrap();
    std::fs::write(root.join(".rdy.yaml"), "pack: rdy-qa\n").unwrap();
    std::fs::write(
        root.join(".rdy/packs/rdy-qa.yaml"),
        rdy_qa::packs::QA_PACK_YAML,
    )
    .unwrap();
    for f in foreign_fixture::store_files(&from) {
        std::fs::copy(from.join(&f), store.join(&f)).unwrap();
    }
    (base, root, from)
}

/// The bytes of every file under `dir`, by relative path.
fn bytes_under(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    foreign_fixture::files_under(dir)
        .into_iter()
        .map(|f| {
            let b = std::fs::read(dir.join(&f)).unwrap();
            (f, b)
        })
        .collect()
}

/// rpl33: the published Release 1 store opens in place. A dry-run upgrade plans nothing and
/// writes nothing. A writable open and reindex pass fsck, add no feature (no `foreign_copy`
/// before first use), append nothing, and read every object key, landing, tag, ref and
/// signature that the fixture records: no identity is rekeyed. The store files keep their bytes.
#[test]
fn rpl33_release_one_store_opens_without_rekey() {
    let (_base, root, from) = release_one_copy();
    let want = read_json(&from.join("expect.json"));
    assert_eq!(
        want["published"],
        json!(true),
        "the published Release 1 store"
    );
    let manifest = &want["store"]["manifest"];
    assert!(!manifest.to_string().contains("foreign_copy"), "{manifest}");
    let store = root.join(".rdy/store");
    let files = bytes_under(&store);

    // the dry run plans nothing and writes nothing
    let plan = rdy_engine::maintenance::run(&root, true).unwrap();
    assert!(plan.dry_run && !plan.changed, "{plan:?}");
    assert!(plan.steps.is_empty(), "{plan:?}");
    assert_eq!(plan.from, plan.to);
    assert_eq!(serde_json::to_value(&plan.to).unwrap(), *manifest);
    assert_eq!(bytes_under(&store), files, "a dry run writes nothing");

    // the writable open: no feature added, nothing appended, every identity as recorded
    let mut r = Rdy::open(&root).unwrap();
    assert_eq!(r.store().access(), StoreAccess::ReadWrite);
    r.reindex().unwrap();
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let features = r.store().features().clone();
    assert_eq!(serde_json::to_value(&features).unwrap(), *manifest);
    let got = foreign_fixture::store_facts(&r);
    for member in [
        "log_end",
        "frames",
        "batches",
        "object_keys",
        "redacted",
        "refs",
        "tags",
        "landings",
        "signatures",
        "raw_encodings",
    ] {
        assert_eq!(got[member], want["store"][member], "{member}");
    }
    r.close().unwrap();
    let after = bytes_under(&store);
    for (f, b) in &files {
        assert_eq!(after.get(f), Some(b), "{f}: the open rewrote it");
    }
    let mut extra: Vec<&String> = after.keys().filter(|f| !files.contains_key(*f)).collect();
    extra.retain(|f| !f.starts_with("index.redb"));
    assert!(extra.is_empty(), "the open wrote {extra:?}");
}

/// The manifest JSON of a `FEATURES` file's text (its second line).
fn features_of(text: &str) -> Value {
    let (first, rest) = text.split_once('\n').unwrap();
    assert_eq!(first, "rdy store features 1");
    serde_json::from_str(rest.trim_end()).unwrap()
}

/// rpl33: an old rdy refuses the new feature and the new stream, by the rules Release 1 reads.
/// Release 1's frozen contract registers seven features, not `foreign_copy`, reads sync format 2
/// only and knows only the purpose `tag_history`. The frozen native store and dump name
/// `foreign_copy` as incompatible with the minimum rdy of the release that carries it, which is
/// newer than Release 1; sentinel 19 and dump format 4 do not move. The frozen delivery's
/// prelude names sync format 3 and its header the purpose `link_delivery`. An unknown
/// incompatible feature refuses an open and a load, and an unknown stream format refuses a read.
#[test]
fn rpl33_old_binary_refuses_foreign_feature_and_stream() {
    let r1 = release_one();
    let registered: Vec<&str> = r1["store"]["registered_names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(registered.len(), 7, "{registered:?}");
    assert!(!registered.contains(&"foreign_copy"));
    assert_eq!(
        r1["store"]["manifest"]["format_text"],
        json!(rdy_store::format::format_file_text())
    );
    assert_eq!(r1["dump_load"]["dump_format"], json!(DUMP_FORMAT));
    // the Release 1 rdy, older than foreign_copy's minimum
    assert!(
        version_key(FOREIGN_COPY_REQUIRED_RDY) > version_key(RELEASE_ONE_RDY),
        "{FOREIGN_COPY_REQUIRED_RDY} {RELEASE_ONE_RDY}"
    );

    // the native store: sentinel 19 and foreign_copy, incompatible, with its minimum rdy
    let native = fixtures().join("store/foreign-copy-1");
    assert_eq!(
        std::fs::read_to_string(native.join("FORMAT")).unwrap(),
        r1["store"]["manifest"]["format_text"].as_str().unwrap()
    );
    let m = features_of(&std::fs::read_to_string(native.join("FEATURES")).unwrap());
    let unknown: Vec<&Value> = m["features"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| !registered.contains(&f["name"].as_str().unwrap()))
        .collect();
    assert_eq!(
        unknown,
        [
            &json!({"name": "foreign_copy", "class": "incompatible", "required_rdy": FOREIGN_COPY_REQUIRED_RDY})
        ],
        "the one feature Release 1 does not know"
    );
    assert!(
        r1["store"]["classes"]
            .as_array()
            .unwrap()
            .contains(&json!("incompatible"))
    );

    // the dump: dump format 4, and the same unknown feature in its header
    let mut f = std::fs::File::open(fixtures().join("dump").join(DUMP)).unwrap();
    let (format, header, _, _) = read_envelope(&mut f).unwrap();
    assert_eq!(format, 4);
    let DumpStore::Features(dm) = header.store else {
        panic!("a feature dump")
    };
    let unknown: Vec<_> = dm
        .features
        .iter()
        .filter(|f| !registered.contains(&f.name.as_str()))
        .map(|f| (f.name.as_str(), f.class.as_str(), f.required_rdy.as_str()))
        .collect();
    assert_eq!(
        unknown,
        [("foreign_copy", "incompatible", FOREIGN_COPY_REQUIRED_RDY)]
    );

    // the delivery: Release 1's magic, then sync format 3, which Release 1 does not read, and a
    // purpose it does not know
    let bytes = std::fs::read(fixtures().join("sync").join(SYNC)).unwrap();
    assert_eq!(
        &bytes[..8],
        r1["sync"]["magic"].as_str().unwrap().as_bytes()
    );
    let format = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    assert_eq!(format, 3);
    assert_ne!(json!(format), r1["sync"]["stream_format"]);
    assert_eq!(r1["sync"]["purposes"], json!([[0, "tag_history"]]));
    let expect = read_json(&fixtures().join("sync").join(SYNC_EXPECT));
    assert_eq!(
        expect["stream"]["header"]["purpose"],
        json!("link_delivery")
    );
    assert_eq!(expect["stream"]["header"]["stream_format"], json!(3));

    // the Release 1 store itself holds no foreign_copy: an old rdy keeps opening it
    let r1_store =
        features_of(&std::fs::read_to_string(fixtures().join("store/feature-1/FEATURES")).unwrap());
    assert!(!r1_store.to_string().contains("foreign_copy"));
}

/// Runs the released rdy `bin` with `args` in `dir`; returns whether it succeeded and its error
/// reply (`--json` puts it on stderr).
fn run_released(bin: &std::ffi::OsStr, dir: &Path, args: &[&str]) -> (bool, Value, String) {
    let out = std::process::Command::new(bin)
        .arg("--json")
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("RDY_NO_DAEMON", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let text = format!("{}{stderr}", String::from_utf8_lossy(&out.stdout));
    let reply = stderr
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
        .unwrap_or(Value::Null);
    (out.status.success(), reply, text)
}

/// rpl33, end to end with an actual released rdy (`RDY_RELEASED_RDY`, the path of its binary:
/// Release 1, rdy 0.47.0, when this test was added). It refuses the frozen native store and the
/// frozen dump with `E_STORE_FORMAT`, naming `foreign_copy` and its minimum rdy, and refuses
/// the frozen delivery's sync format 3 before any record. It changes no byte of the store and
/// writes no store for the refused load. It needs a released binary, so the gate skips it; run
/// it by hand: `RDY_RELEASED_RDY=<path> cargo test -p rdy-qa --release --test release
/// rpl33_a_released_rdy -- --ignored`.
#[test]
#[ignore = "needs RDY_RELEASED_RDY, the path of a released rdy binary"]
fn rpl33_a_released_rdy_refuses_foreign_feature_and_stream() {
    let bin = std::env::var_os("RDY_RELEASED_RDY").expect("RDY_RELEASED_RDY names a released rdy");
    let version = std::process::Command::new(&bin)
        .arg("--version")
        .output()
        .unwrap();
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();
    eprintln!("released rdy: {version}");
    let v = version.strip_prefix("rdy ").expect("rdy <version>");
    assert!(
        version_key(v) < version_key(FOREIGN_COPY_REQUIRED_RDY),
        "an rdy older than {FOREIGN_COPY_REQUIRED_RDY}: {version}"
    );
    let need = format!("store feature foreign_copy needs rdy {FOREIGN_COPY_REQUIRED_RDY}");

    // the native store: refused at open, no byte changed
    let (_base, root) = foreign_fixture::project_copy(&fixtures().join("store/foreign-copy-1"));
    let before = bytes_under(&root.join(".rdy"));
    let (ok, reply, text) = run_released(&bin, &root, &["status", "--no-daemon"]);
    eprintln!("status: {text}");
    assert!(!ok, "{text}");
    assert_eq!(reply["code"], "E_STORE_FORMAT", "{text}");
    assert_eq!(reply["message"].as_str(), Some(need.as_str()), "{text}");
    assert_eq!(bytes_under(&root.join(".rdy")), before, "no byte changed");

    // the dump: refused, and no store written
    let scratch = tempfile::tempdir().unwrap();
    let to = scratch.path().join("loaded");
    let dump = fixtures().join("dump").join(DUMP);
    let (ok, reply, text) = run_released(
        &bin,
        scratch.path(),
        &["load", dump.to_str().unwrap(), to.to_str().unwrap()],
    );
    eprintln!("load: {text}");
    assert!(!ok, "{text}");
    assert_eq!(reply["code"], "E_STORE_FORMAT", "{text}");
    assert_eq!(reply["message"].as_str(), Some(need.as_str()), "{text}");
    assert!(
        !to.join(".rdy/store").exists(),
        "the refused load wrote a store"
    );

    // the delivery: Release 1 reads sync format 2 only, so it refuses format 3 at the prelude
    // (record 0, byte 8) and reads no record
    let sync = fixtures().join("sync").join(SYNC);
    let (ok, reply, text) = run_released(
        &bin,
        scratch.path(),
        &["stream", "check", sync.to_str().unwrap()],
    );
    eprintln!("stream check: {text}");
    assert!(!ok, "{text}");
    assert_eq!(
        (reply["code"].as_str(), reply["message"].as_str()),
        (
            Some("E_BAD_RECORD"),
            Some("stream record 0 at byte 8: invalid header")
        ),
        "{text}"
    );
}
