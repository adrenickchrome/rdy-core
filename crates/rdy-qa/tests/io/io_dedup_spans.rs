//! r5902 (D-1ER84): the store feature `dedup_spans` (incompatible, minimum rdy
//! [`DEDUP_SPANS_REQUIRED_RDY`]) and its span map. A new store lists the feature from its first
//! batch; the map is local to each store, so no portable container carries it, and every
//! receiver builds its own at its own places on the next compact.
//!
//! The frozen members, each written once by [`r5902_write_dedup_fixture_once`] and never
//! rewritten:
//!
//! - the native store `fixtures/store/dedup-spans-1/`: `FEATURES`, `FORMAT`, `SPANMAP`, `lock`
//!   and one segment, byte for byte, and no derived index. Its history was written by the last
//!   rdy before snapshot deltas (two landings, so two old SNAPSHOT records), upgraded by this
//!   rdy's first writer open (which mapped that history after its backup), then given an
//!   answer-only plan chain (two old PLAN versions after the bound one) and a change that
//!   creates a large block, then compacted before the map was frozen. Its `expect.json` records
//!   every logical object key, the store facts an open reads, and the mapped bytes by class.
//! - the dump `fixtures/dump/v4-f1-dedup-spans.rdydump` of the same project and its expect file
//!   (`io_feature_fixture` registers the pair in `FIRST_USE`). The dump holds the logical frames
//!   and no map bytes.
use rdy_engine::clone::{CloneSource, clone, clone_at};
use rdy_engine::stream_take::take;
use rdy_engine::{Rdy, Scope};
use rdy_model::plan::{PlanAct, PlanAnswer, PlanKind, item_hash};
use rdy_model::{BlockId, Hash, Plan, Provenance, Role};
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::foreign_fixture;
use rdy_store::dedup::{MapOpen, SPANMAP_FILE, SpanMap};
use rdy_store::format::{
    DEDUP_SPANS_REQUIRED_RDY, FEATURE_DEDUP_SPANS, FeatureClass, registered_features,
};
use rdy_store::kinds;
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The blake3 of the inventory text of the native store files, set when they were written. A
/// change here needs a founder decision, and after publication it is never allowed.
const INVENTORY_BLAKE3: &str = "75981a5be6cddb0c5b5b6b1174a616a59b00278cb8d84d4a713d942df1d47be8";
/// The blake3 of the native store's `expect.json`, set when it was written.
const EXPECT_BLAKE3: &str = "8229c53364d906cc57d60cb47b2702eca0711bd88400b25956adac20bf96ec0a";

const DUMP: &str = "v4-f1-dedup-spans.rdydump";
const DUMP_EXPECT: &str = "v4-f1-dedup-spans.expect.json";
const MAGIC: &[u8] = b"RDYSPANS";

/// The tag of the world's last landing.
const TAG: &str = "v1.0.0";
/// The prose blocks of the doc: enough equal SNAPSHOT entries for whole mapped blocks.
const PROSE: usize = 200;
/// The answers of the plan chain's first answer-only version.
const ANSWERS: usize = 64;

fn qa_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fixture_dir() -> PathBuf {
    qa_dir().join("fixtures/store/dedup-spans-1")
}

fn dump_path(name: &str) -> PathBuf {
    qa_dir().join("fixtures/dump").join(name)
}

/// The eight members the writer creates, in the order it creates them.
fn members() -> Vec<PathBuf> {
    let d = fixture_dir();
    vec![
        d.join("FEATURES"),
        d.join("FORMAT"),
        d.join("SPANMAP"),
        d.join("lock"),
        d.join("segments/seg-000000.log"),
        d.join("expect.json"),
        dump_path(DUMP),
        dump_path(DUMP_EXPECT),
    ]
}

/// None when no member exists, else the exact refusal of the writer.
fn written_once(paths: &[PathBuf]) -> Result<(), String> {
    match paths.iter().find(|p| p.exists()) {
        Some(p) => Err(format!(
            "dedup fixtures are written once; output exists: {}",
            p.display()
        )),
        None => Ok(()),
    }
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn pretty(v: &Value) -> Vec<u8> {
    (serde_json::to_string_pretty(v).unwrap() + "\n").into_bytes()
}

/// Writes `bytes` to the new file `p` (create_new); refuses a file that exists.
fn write_new(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .unwrap_or_else(|e| {
            panic!(
                "dedup fixtures are written once; output exists: {}: {e}",
                p.display()
            )
        });
    std::io::Write::write_all(&mut out, bytes).unwrap();
    out.sync_all().unwrap();
}

/// `n` bytes of text that repeat no 4,096-byte window, from `seed`.
fn text(n: usize, seed: u64) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + (x % 26) as u8)
        })
        .collect()
}

fn ids(w: &rdy_engine::WriteResult) -> BTreeMap<String, String> {
    w.created.clone().into_iter().collect()
}

/// The world the members hold.
struct World {
    _base: tempfile::TempDir,
    root: PathBuf,
    doc: String,
    edited: String,
    big: String,
    landings: Vec<String>,
    plans: Vec<String>,
}

fn agent() -> Provenance {
    Provenance {
        actor: "agent:plan".into(),
        run: Some("plan".into()),
        role: Role::Writer,
        at_ms: 2,
    }
}

/// An answer-only plan chain of view `view`: the bound version, then `ANSWERS` confirmed items,
/// then the same answers and one more. Returns the three version ids.
fn plan_chain(r: &mut Rdy, view: &str, parent: &str) -> Vec<String> {
    write(
        r,
        view,
        json!([{"op": "create", "type": "prose", "parent": parent,
                "body": "The plan's own block."}]),
    )
    .unwrap();
    r.invalidate();
    let proposal = r
        .store_mut()
        .propose(view, "Plans the doc", agent())
        .unwrap();
    let bound = Plan {
        stable: None,
        prev: None,
        view: view.into(),
        proposal: Some(proposal),
        answers: vec![],
        heads: vec![],
        provenance: agent(),
        seal: None,
    };
    r.store_mut()
        .write_guarded_for_tests(rdy_qa::every_kind::plan_frames(&bound))
        .unwrap();
    r.invalidate();
    let cause = BlockId::parse("b-00001").unwrap();
    let answer = |i: usize| PlanAnswer {
        item: item_hash(&cause, &format!("app:src/part{i:03}.rs"), PlanKind::Confirm),
        act: PlanAct::Confirm,
        reason: Some(format!(
            "part {i:03} still follows the decision as it is written today"
        )),
        subject: Some(Hash([0x51; 32])),
        cause: Hash([0x52; 32]),
        by: agent(),
    };
    let mut answers: Vec<PlanAnswer> = (0..ANSWERS).map(answer).collect();
    answers.sort_by_key(|a| a.item);
    let first = Plan {
        stable: Some(bound.id()),
        prev: Some(bound.id()),
        proposal: None,
        answers: answers.clone(),
        ..bound.clone()
    };
    let first = r.store_mut().put_plan(&first).unwrap();
    answers.push(answer(ANSWERS));
    answers.sort_by_key(|a| a.item);
    let second = Plan {
        stable: Some(bound.id()),
        prev: Some(first),
        proposal: None,
        answers,
        ..bound.clone()
    };
    let second = r.store_mut().put_plan(&second).unwrap();
    r.invalidate();
    vec![
        bound.id().to_string(),
        first.to_string(),
        second.to_string(),
    ]
}

fn build_world() -> World {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    let (doc, edited, section, landings) = {
        // the history the last rdy before snapshot deltas wrote: each landing a full SNAPSHOT
        let _legacy = rdy_store::format::legacy_writer_for_tests();
        let mut r = Rdy::init(&root, "rdy-standard").unwrap();
        let mut ops = vec![
            json!({"op": "create", "type": "doc", "id": "$d",
                   "fields": {"title": "Spans", "doc_type": "page"}}),
            json!({"op": "create", "type": "section", "id": "$s", "parent": "$d",
                   "fields": {"title": "Terms"}}),
        ];
        for i in 0..PROSE {
            ops.push(
                json!({"op": "create", "type": "prose", "id": format!("$p{i}"),
                            "parent": "$s", "body": format!("Term {i} is stored once.")}),
            );
        }
        let w = write(&mut r, "w0", Value::Array(ops)).unwrap();
        let first = land(&mut r, "w0").unwrap();
        let doc = ids(&w)["$d"].clone();
        let section = ids(&w)["$s"].clone();
        let edited = ids(&w)["$p0"].clone();
        write(
            &mut r,
            "w1",
            json!([{"op": "update", "id": edited, "body": "Term 0 is stored once, and kept."}]),
        )
        .unwrap();
        let second = land(&mut r, "w1").unwrap();
        r.tag(&approver(), TAG, None, None).unwrap();
        r.close().unwrap();
        (doc, edited, section, vec![first, second])
    };
    // this rdy's writer open: the upgrade maps the old SNAPSHOT pair after its backup
    let mut r = Rdy::open(&root).unwrap();
    let plans = plan_chain(&mut r, "w2", &section);
    let w = write(
        &mut r,
        "w3",
        json!([{"op": "create", "type": "prose", "id": "$big", "parent": section,
                "body": text(20_000, 7)}]),
    )
    .unwrap();
    let big = ids(&w)["$big"].clone();
    let report = r.compact(false).unwrap();
    assert!(report.deduplicated_bytes > 0, "{report:?}");
    r.close().unwrap();
    World {
        _base: base,
        root,
        doc,
        edited,
        big,
        landings,
        plans,
    }
}

fn world_json(w: &World) -> Value {
    json!({
        "doc": w.doc,
        "edited": w.edited,
        "big": w.big,
        "landings": w.landings,
        "plans": w.plans,
        "tag": TAG,
    })
}

/// The mapped target bytes of the map in `store` by class, and its row count.
fn span_facts(store: &Path) -> Value {
    let map = SpanMap::load(store, MapOpen::ReadOnly).unwrap();
    let mut by = BTreeMap::from([("change", 0u64), ("plan", 0), ("snapshot", 0)]);
    for m in map.mappings() {
        let class = match m.target.raw_kind & !rdy_store::codec::VERSIONED {
            k if k == kinds::CHANGE => "change",
            k if k == kinds::PLAN => "plan",
            k if k == kinds::SNAPSHOT => "snapshot",
            k => panic!("a mapped target of kind {k}"),
        };
        *by.get_mut(class).unwrap() += u64::from(m.target.len);
    }
    json!({
        "rows": map.rows(),
        "through": map.through().map(|l| json!([l.segment, l.offset])),
        "mapped": by,
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

/// The open facts of the project at `root`: rebuilt index, full fsck, store facts and the byte
/// report's deduplicated bytes.
fn opened_facts(root: &Path) -> Value {
    let mut r = Rdy::open(root).unwrap();
    r.reindex().unwrap();
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let mut facts = foreign_fixture::store_facts(&r);
    facts["deduplicated_bytes"] = json!(r.store().log_bytes().unwrap().deduplicated_bytes);
    r.close().unwrap();
    facts
}

/// Writes the native store, its expect file, the dump and its expect file once, from one world.
/// Run by hand, never by the gate, after the readers fail: `cargo test -p rdy-qa --test io
/// r5902_write_dedup_fixture_once -- --ignored`. It refuses when any member exists.
#[test]
#[ignore]
fn r5902_write_dedup_fixture_once() {
    if let Err(e) = written_once(&members()) {
        panic!("{e}");
    }
    let dir = fixture_dir();
    let w = build_world();
    let store = w.root.join(".rdy/store");
    let copied = foreign_fixture::store_files(&store);
    assert_eq!(
        copied,
        [
            "FEATURES",
            "FORMAT",
            "SPANMAP",
            "lock",
            "segments/seg-000000.log"
        ],
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
        "required_rdy": DEDUP_SPANS_REQUIRED_RDY,
        "world": world_json(&w),
        "inventory": foreign_fixture::inventory(&dir),
        "spans": span_facts(&dir),
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
    let r = Rdy::open(&w.root).unwrap();
    r.read().dump(&dumped).unwrap();
    r.close().unwrap();
    let dump_bytes = std::fs::read(&dumped).unwrap();
    assert!(!contains(&dump_bytes, MAGIC), "the dump holds no map bytes");
    write_new(&dump_path(DUMP), &dump_bytes);
    let v = json!({
        "published": false,
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": DEDUP_SPANS_REQUIRED_RDY,
        "formats": rdy_qa::chain_fixture::current_formats(),
        "world": world_json(&w),
        "file": foreign_fixture::dump_file_facts(&dump_path(DUMP)),
        "whole": foreign_fixture::dump_load_facts(&dump_path(DUMP), &scratch.path().join("loaded")),
    });
    let bytes = pretty(&v);
    write_new(&dump_path(DUMP_EXPECT), &bytes);
    eprintln!("{DUMP} blake3 {}", Hash::of(&dump_bytes));
    eprintln!("{DUMP_EXPECT} blake3 {}", Hash::of(&bytes));
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
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

/// r5902: the feature is registered as incompatible at 0.59.0 and frozen in `FIRST_USE`; the
/// native store reopens, reindexes and passes fsck with its recorded facts, map and logical
/// reads, and keeps its files; an older reader refuses it; its dump loads to the recorded
/// logical store with no map bytes; and the writer refuses to write any member again.
#[test]
fn r5902_fixture_is_local_and_immutable() {
    let reg = registered_features()
        .into_iter()
        .find(|f| f.name == FEATURE_DEDUP_SPANS)
        .unwrap();
    assert_eq!(reg.class, FeatureClass::Incompatible);
    assert_eq!(reg.required_rdy, DEDUP_SPANS_REQUIRED_RDY);
    assert_eq!(DEDUP_SPANS_REQUIRED_RDY, "0.59.0");
    let first_use = crate::io_feature_fixture::dump_members();
    assert!(first_use.contains(&DUMP) && first_use.contains(&DUMP_EXPECT));

    let dir = fixture_dir();
    let before = source_digests();
    let want = read_json(&dir.join("expect.json"));
    assert_eq!(
        foreign_fixture::files_under(&dir),
        [
            "FEATURES",
            "FORMAT",
            "SPANMAP",
            "expect.json",
            "lock",
            "segments/seg-000000.log"
        ],
        "one segment and no derived index"
    );
    assert_eq!(want["inventory"], foreign_fixture::inventory(&dir));
    assert_eq!(
        Hash::of(foreign_fixture::inventory_text(&dir).as_bytes()).to_string(),
        INVENTORY_BLAKE3
    );
    assert_eq!(
        foreign_fixture::digest_of(&dir.join("expect.json")).1,
        EXPECT_BLAKE3
    );
    assert_eq!(want["required_rdy"], json!(DEDUP_SPANS_REQUIRED_RDY));
    let written = std::fs::read_to_string(dir.join("expect.json")).unwrap();
    for bad in ["/Users", "/home/", "/tmp", "/var/", "\\\\", ":\\"] {
        assert!(!written.contains(bad), "the expect file names {bad}");
    }
    // the map holds all three classes, and the recorded counts
    let spans = span_facts(&dir);
    assert_eq!(spans, want["spans"]);
    for class in ["change", "plan", "snapshot"] {
        assert!(
            spans["mapped"][class].as_u64().unwrap() >= 4096,
            "{class}: {spans}"
        );
    }

    // a copy opens with no upgrade step and reads every recorded fact
    let (_base, root) = foreign_fixture::project_copy(&dir);
    let plan = rdy_store::upgrade::inspect(&root.join(".rdy/store")).unwrap();
    assert!(plan.steps.is_empty(), "no step: {:?}", plan.steps);
    let store = opened_facts(&root);
    for member in want["store"].as_object().unwrap().keys() {
        assert_eq!(store[member], want["store"][member], "{member}");
    }
    let mut r = Rdy::open(&root).unwrap();
    let world = &want["world"];
    let big = BlockId::parse(world["big"].as_str().unwrap()).unwrap();
    let body = r
        .read()
        .block(&Scope::View("w3".into()), &big)
        .unwrap()
        .body;
    assert_eq!(body, text(20_000, 7), "the mapped block reads whole");
    for p in world["plans"].as_array().unwrap() {
        let h: Hash = p.as_str().unwrap().parse().unwrap();
        r.store().reader().plan(&h).unwrap();
    }
    r.compact(false).unwrap();
    r.reindex().unwrap();
    assert_eq!(
        foreign_fixture::store_facts(&r)["object_keys"],
        want["store"]["object_keys"],
        "every logical hash survives"
    );
    r.close().unwrap();
    rdy_qa::reindex_check::reindex_reproduces(&root).unwrap_or_else(|v| panic!("reindex: {v:#?}"));

    // an older rdy, which does not know the feature, refuses the store
    {
        let (_b, older) = foreign_fixture::project_copy(&dir);
        let _old = rdy_store::format::older_reader_for_tests(&[FEATURE_DEDUP_SPANS]);
        let e = Rdy::open(&older).err().expect("an older rdy refuses it");
        assert_eq!(e.code, "E_STORE_FORMAT", "{e:?}");
        assert!(
            e.message
                .contains("store feature dedup_spans needs rdy 0.59.0"),
            "{e:?}"
        );
    }

    // the dump holds the logical frames and no map; its load builds no map
    let dump = dump_path(DUMP);
    let dwant = read_json(&dump_path(DUMP_EXPECT));
    assert_eq!(dwant["required_rdy"], json!(DEDUP_SPANS_REQUIRED_RDY));
    assert!(!contains(&std::fs::read(&dump).unwrap(), MAGIC));
    assert_eq!(foreign_fixture::dump_file_facts(&dump), dwant["file"]);
    let scratch = tempfile::tempdir().unwrap();
    let to = scratch.path().join("loaded");
    assert_eq!(foreign_fixture::dump_load_facts(&dump, &to), dwant["whole"]);
    assert_eq!(
        dwant["whole"]["store"]["object_keys"], want["store"]["object_keys"],
        "the load reconstructs every logical object"
    );
    let loaded = SpanMap::load(&to.join(".rdy/store"), MapOpen::ReadOnly).unwrap();
    assert_eq!(loaded.mappings().count(), 0, "a load maps nothing");

    // the writer refuses every member, with its exact text
    let refused = written_once(&members()).unwrap_err();
    assert_eq!(
        refused,
        format!(
            "dedup fixtures are written once; output exists: {}",
            members()[0].display()
        )
    );
    assert_eq!(
        source_digests(),
        before,
        "the checked-in files never change"
    );
}

// ---------------------------------------------------------------------------------------------
// logical bytes and receivers

/// A current project whose history holds a mapped CHANGE/BLOCK pair, tagged. With `blob`,
/// figures of the tagged history also hold blobs of that many distinct bytes in all, so the log
/// and the tag's history are past the threshold of unmapped bytes.
fn source(blob: usize) -> (tempfile::TempDir, PathBuf, String) {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("src");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d",
               "fields": {"title": "Spans", "doc_type": "page"}}),
        json!({"op": "create", "type": "prose", "id": "$big", "parent": "$d",
               "body": text(20_000, 3)}),
    ];
    if blob > 0 {
        let section = json!({"op": "create", "type": "section", "id": "$s", "parent": "$d",
                             "fields": {"title": "Figures"}});
        ops.push(section);
        // a publication blob holds at most 64 MiB, so the bytes go in blobs of 32 MiB at most
        let mut left = blob;
        let mut seed = 9;
        while left > 0 {
            let n = left.min(32 << 20);
            let h = r.store_mut().put_blob(text(n, seed).as_bytes()).unwrap();
            ops.push(json!({"op": "create", "type": "figure", "parent": "$s",
                            "fields": {"blob": h.to_string(), "media_type": "image/png",
                                       "alt": format!("fig {seed}")}}));
            left -= n;
            seed += 1;
        }
    }
    let w = write(&mut r, "w0", json!(ops)).unwrap();
    land(&mut r, "w0").unwrap();
    r.tag(&approver(), TAG, None, None).unwrap();
    let big = ids(&w)["$big"].clone();
    r.close().unwrap();
    (base, root, big)
}

fn rows(root: &Path) -> u64 {
    SpanMap::load(&root.join(".rdy/store"), MapOpen::ReadOnly)
        .unwrap()
        .rows()
}

fn spanmap_len(root: &Path) -> Option<u64> {
    std::fs::metadata(root.join(".rdy/store").join(SPANMAP_FILE))
        .ok()
        .map(|m| m.len())
}

/// r5902: the bytes rdy reads, the dump bytes and the sync batch bytes are the same before and
/// after physical mapping, with no log write between; fsck and a reindex pass after it; and a
/// damaged retained source byte fails the existing hash check.
#[test]
fn r5902_logical_reads_and_exports_are_identical() {
    let (_base, root, big) = source(0);
    let scratch = tempfile::tempdir().unwrap();
    let id = BlockId::parse(&big).unwrap();
    let exports = |r: &Rdy, n: &str| {
        let p = scratch.path().join(format!("{n}.rdydump"));
        r.read().dump(&p).unwrap();
        let (t, _) = r
            .store()
            .tags()
            .unwrap()
            .into_iter()
            .find(|(name, ..)| name == TAG)
            .map(|(_, t, rec)| (t, rec))
            .unwrap();
        let batch = rdy_qa::every_kind::export_tag(r, t);
        (std::fs::read(p).unwrap(), batch)
    };
    let mut r = Rdy::open(&root).unwrap();
    let block = r.read().block(&Scope::Canon, &id).unwrap();
    let facts = foreign_fixture::store_facts(&r);
    let (dump, batch) = exports(&r, "before");
    let report = r.compact(false).unwrap();
    assert!(report.deduplicated_bytes >= 4096, "{report:?}");
    assert_eq!(r.read().block(&Scope::Canon, &id).unwrap(), block);
    assert_eq!(foreign_fixture::store_facts(&r), facts);
    let (dump2, batch2) = exports(&r, "after");
    assert_eq!(dump2, dump, "the dump bytes stay the same");
    assert_eq!(batch2, batch, "the sync batch bytes stay the same");
    assert!(!contains(&dump2, MAGIC) && !contains(&batch2, MAGIC));
    r.fsck().unwrap();
    r.reindex().unwrap();
    r.close().unwrap();
    let r = Rdy::open(&root).unwrap();
    assert_eq!(r.read().block(&Scope::Canon, &id).unwrap(), block);
    r.close().unwrap();

    // a damaged byte of a mapped range's retained source fails the hash check
    let store = root.join(".rdy/store");
    let map = SpanMap::load(&store, MapOpen::ReadOnly).unwrap();
    let m = *map.mappings().next().expect("a mapping");
    let seg = rdy_store::segment::seg_path(&store.join("segments"), m.source.place.segment);
    let mut bytes = std::fs::read(&seg).unwrap();
    let at = (m.source.physical_start() + 7) as usize;
    bytes[at] ^= 0x20;
    std::fs::write(&seg, bytes).unwrap();
    let damaged = Rdy::open(&root).and_then(|r| r.fsck().map(|_| ()));
    assert!(damaged.is_err(), "the hash check fails");
}

/// r5902: load, clone, a fork's project take and a holder's take append no map row and punch no
/// mapped range, even with the source past the threshold; the next compact builds each local
/// map at its own places, and every original object key survives.
#[test]
fn r5902_receivers_build_local_maps() {
    let (_base, root, big) = source(rdy_store::dedup::THRESHOLD_BYTES as usize + (1 << 20));
    let id = BlockId::parse(&big).unwrap();
    {
        let r = Rdy::open(&root).unwrap();
        r.compact(false).unwrap();
        r.close().unwrap();
    }
    assert!(rows(&root) > 0, "the source has its own map");
    let r = Rdy::open(&root).unwrap();
    let keys = foreign_fixture::store_facts(&r)["object_keys"].clone();
    let block = r.read().block(&Scope::Canon, &id).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let dump = scratch.path().join("src.rdydump");
    r.read().dump(&dump).unwrap();
    assert!(!contains(&std::fs::read(&dump).unwrap(), MAGIC));
    let (t, landing) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == TAG)
        .map(|(_, t, rec)| (t, rec.landing))
        .unwrap();
    let batch = rdy_qa::every_kind::export_tag(&r, t);
    let repo = r.store().reader().repo_id().unwrap().unwrap();
    let mut made = vec![];
    let to = scratch.path().join("loaded");
    rdy_engine::load::load(&dump, &to).unwrap();
    made.push(("load", to));
    let to = scratch.path().join("clone");
    clone(CloneSource::State(&r.read()), &to, false).unwrap();
    made.push(("clone", to));
    let to = scratch.path().join("fork");
    clone_at(CloneSource::State(&r.read()), &to, true, Some(TAG)).unwrap();
    made.push(("fork", to));
    r.close().unwrap();
    for (what, to) in &made {
        // no row, and no Through: the header at most
        assert!(
            spanmap_len(to).is_none_or(|n| n == 12),
            "{what}: {:?}",
            spanmap_len(to)
        );
        let r = Rdy::open(to).unwrap();
        let facts = foreign_fixture::store_facts(&r);
        for k in keys.as_array().unwrap() {
            if *what != "fork" {
                assert!(
                    facts["object_keys"].as_array().unwrap().contains(k),
                    "{what}: {k}"
                );
            }
        }
        assert_eq!(r.read().block(&Scope::Canon, &id).unwrap(), block, "{what}");
        let report = r.compact(false).unwrap();
        assert!(report.deduplicated_bytes >= 4096, "{what}: {report:?}");
        r.close().unwrap();
        let map = SpanMap::load(&to.join(".rdy/store"), MapOpen::ReadOnly).unwrap();
        assert!(map.mappings().count() > 0, "{what}: a local map");
        let r = Rdy::open(to).unwrap();
        assert_eq!(r.read().block(&Scope::Canon, &id).unwrap(), block, "{what}");
        r.close().unwrap();
    }

    // a holder's take of the tag's history
    let p = scratch.path().join("up.rdysync");
    std::fs::write(&p, &batch).unwrap();
    let holder = scratch.path().join("holder");
    let mut h = Rdy::create_holder(&holder, repo).unwrap();
    take(
        &mut h,
        &[File::open(&p).unwrap()],
        UploadTarget {
            tag: TAG.into(),
            landing,
        },
    )
    .unwrap();
    h.close().unwrap();
    assert!(spanmap_len(&holder).is_none_or(|n| n == 12), "holder");
    // the take was past the threshold of unmapped bytes, and the holder's own compact then
    // builds its local map, reads back every byte and keeps every original key
    let store = holder.join(".rdy/store");
    let log: u64 = std::fs::read_dir(store.join("segments"))
        .unwrap()
        .filter_map(|e| e.ok()?.metadata().ok())
        .map(|m| m.len())
        .sum();
    assert!(log >= rdy_store::dedup::THRESHOLD_BYTES, "holder log {log}");
    let h = Rdy::open_holder(&holder).unwrap();
    let held = foreign_fixture::store_facts(&h)["object_keys"].clone();
    let report = h.compact(false).unwrap();
    assert!(report.deduplicated_bytes >= 4096, "holder: {report:?}");
    h.fsck().unwrap();
    h.close().unwrap();
    let map = SpanMap::load(&store, MapOpen::ReadOnly).unwrap();
    assert!(map.mappings().count() > 0, "holder: a local map");
    let mut h = Rdy::open_holder(&holder).unwrap();
    assert_eq!(foreign_fixture::store_facts(&h)["object_keys"], held);
    h.fsck().unwrap();
    h.reindex().unwrap();
    h.close().unwrap();
}
