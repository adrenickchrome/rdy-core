//! ec13: the dump promise for store format 12 (D-69AY3, D-FR0C4). The quality gate keeps one
//! fixture for each pair of dump format and store format: `(1, 12)` is released at the merge of
//! Export and clone, with this file. Every later rdy loads it with every hash kept. Write it once:
//! never rewrite it.
//!
//! The fixture is a dump of the every-kind project, which is a clone (O41): it holds the refs the
//! store format adds, `meta/repo_id` (the source's, kept by the clone) and `meta/origin`. As the
//! earlier fixtures, it was written on a store whose segments hold `FIXTURE_SEGMENT_MAX` bytes, so
//! the load must put each batch at its source place.
//!
//! A later store format may add kinds and refs, never re-encode (D-F0904). A load of this fixture
//! then appends one batch after the fixture's log: it sets `meta/store_format` to that rdy's
//! store format (FW-UP1). The fixture holds its repo id, so the batch holds no `meta/repo_id`
//! (RULING F2 (a)). The load test allows exactly that batch.
use rdy_engine::{Rdy, load::load};
use rdy_qa::every_kind;
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader};
use rdy_store::format::{REF_STORE_FORMAT, STORE_FORMAT, encode_u32};
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::meta::{REF_ORIGIN, REF_REPO_ID};
use rdy_store::segment::Location;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The fixture's blake3, set when it was written. A change here needs a founder decision.
const FIXTURE_BLAKE3: &str = "c41ca2a0206997886d1f8f6439a4d2c741a472d41ae63edbae82ad13d869d9b7";
/// The blake3 of the expect file's bytes, set when it was written. A change here needs a founder
/// decision.
const EXPECT_BLAKE3: &str = "aa203fb8f0b109f27726ddfe15fe91182f969747d8bf38c5a01692b2a4804de0";
/// The ref families of the v1-s12 fixture (store format 12): the fourteen of the v1-s11 fixture,
/// `meta/repo_id` and `meta/origin`. Frozen with this fixture: a later store format that adds a
/// family adds it to `every_kind::FAMILIES`, never here.
const V12_FAMILIES: [&str; 16] = [
    "meta/store_format",
    "meta/repo_id",
    "meta/origin",
    "view/",
    "tag/",
    "intent/",
    "alt/",
    "dismiss/",
    "mention/",
    "pair/",
    "semantic/",
    "inbox/",
    "canon",
    "proposal/",
    "answer/",
    "pending/",
];
/// The segment size limit of the store the fixture was written from.
const FIXTURE_SEGMENT_MAX: u64 = 4096;
/// The id seed of the fixture's writer.
const FIXTURE_ID_SEED: u64 = 12;
const DUMP: &str = "v1-s12.rdydump";
const EXPECT: &str = "v1-s12.expect.json";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name)
}

fn expect() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(EXPECT)).unwrap()).unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The dump at `p`: its batches in order, the source log end its header names, and its store
/// format.
fn dump_batches(p: &Path) -> (Vec<DumpBatch>, Location, u32) {
    let (mut rd, _, h) = DumpReader::open(std::fs::File::open(p).unwrap()).unwrap();
    let mut out = vec![];
    while let Some(item) = rd.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            out.push(b);
        }
    }
    (out, h.log_end, h.store_format)
}

/// Each batch's start as `[segment, offset]`, in log order.
fn starts(bs: impl IntoIterator<Item = Location>) -> serde_json::Value {
    json!(
        bs.into_iter()
            .map(|l| [u64::from(l.segment), l.offset])
            .collect::<Vec<_>>()
    )
}

/// Writes the fixture once. Run by hand, never by the gate:
/// `cargo test -p rdy-qa --release --test io ec13_write_the_v1_s12_fixture -- --ignored`.
#[test]
#[ignore]
fn ec13_write_the_v1_s12_fixture() {
    let (dump, expect) = (fixture(DUMP), fixture(EXPECT));
    assert!(
        !dump.exists() && !expect.exists(),
        "the v1-s12 fixture is written once: never rewrite it"
    );
    assert_eq!(STORE_FORMAT, 12, "the v1-s12 fixture holds store format 12");
    std::fs::create_dir_all(dump.parent().unwrap()).unwrap();
    rdy_model::seed_ids_for_tests(FIXTURE_ID_SEED);
    let src = every_kind::rolling_project(FIXTURE_SEGMENT_MAX);
    rdy_model::unseed_ids_for_tests();
    let r = Rdy::open(src.path()).unwrap();
    r.read().dump(&dump).unwrap();
    let mut facts = every_kind::facts(&r);
    r.close().unwrap();
    facts["segment_max"] = json!(FIXTURE_SEGMENT_MAX);
    facts["batch_starts"] = starts(dump_batches(&dump).0.iter().map(|b| b.start));
    let text = serde_json::to_string_pretty(&facts).unwrap() + "\n";
    std::fs::write(&expect, &text).unwrap();
    eprintln!(
        "blake3 {}",
        rdy_model::Hash::of(&std::fs::read(&dump).unwrap())
    );
    eprintln!("expect blake3 {}", rdy_model::Hash::of(text.as_bytes()));
}

#[test]
fn ec13_the_v1_s12_fixture_is_never_rewritten() {
    // A change to either file needs a founder decision.
    let bytes = std::fs::read(fixture(DUMP)).unwrap();
    assert_eq!(rdy_model::Hash::of(&bytes).to_string(), FIXTURE_BLAKE3);
    let bytes = std::fs::read(fixture(EXPECT)).unwrap();
    assert_eq!(rdy_model::Hash::of(&bytes).to_string(), EXPECT_BLAKE3);
}

#[test]
fn ec13_the_v1_s12_fixture_loads_with_every_hash() {
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    let s = load(&fixture(DUMP), &dir).unwrap();
    assert_eq!(s.dump_format, 1);
    assert_eq!(s.store_format, 12, "the fixture's store format");
    let expect = expect();
    let (batches, log_end, fixture_format) = dump_batches(&fixture(DUMP));
    assert_eq!(fixture_format, 12);
    let r = Rdy::open(&dir).unwrap();
    let got = every_kind::facts(&r);
    assert_eq!(
        got["landings"], expect["landings"],
        "every landing, in order"
    );
    // every ref holds its value, and no other ref exists; `meta/store_format` names this rdy's
    // store format (the upgrade batch sets it when the fixture's format is older). The fixture
    // holds its repo id and its origin: a load keeps both.
    let mut want = expect["refs"].clone();
    want[REF_STORE_FORMAT] = json!(hex(&encode_u32(STORE_FORMAT)));
    assert!(want[REF_REPO_ID].is_string(), "the fixture holds a repo id");
    assert!(want[REF_ORIGIN].is_string(), "the fixture holds an origin");
    assert_eq!(
        got["refs"], want,
        "every ref: repo id, origin, views, tags, proposals, inboxes, reviews, answers, snoozes"
    );
    // the fixture's own log range: every kind counted, and each batch at its source place
    let reader = r.store().reader();
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    let mut objects: BTreeMap<String, u64> = BTreeMap::new();
    reader
        .for_each_frame_from(origin, log_end, |_, kind, _, _, _| {
            if kind != kinds::BATCH_COMMIT {
                *objects.entry(kind.to_string()).or_default() += 1;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(json!(objects), expect["objects"], "every kind, counted");
    let mut loaded = vec![];
    reader
        .for_each_batch(log_end, |b| {
            loaded.push(b.start);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        starts(loaded),
        expect["batch_starts"],
        "each batch at its source place"
    );
    // past the fixture's log end: nothing while the store format is 12; for a newer store format
    // only the upgrade batch, which holds `meta/store_format` alone (RULING F2 (a))
    let mut tail = vec![];
    reader
        .for_each_frame_from(log_end, r.store().log_end(), |_, kind, _, payload, _| {
            tail.push((kind, payload.to_vec()));
            Ok(())
        })
        .unwrap();
    if STORE_FORMAT == fixture_format {
        assert!(tail.is_empty(), "nothing past the fixture's log: {tail:?}");
    } else {
        assert_eq!(tail.len(), 2, "one ref frame and its marker: {tail:?}");
        assert_eq!(tail[0].0, kinds::REF_UPDATE);
        let u: RefUpdate = postcard::from_bytes(&tail[0].1).unwrap();
        assert_eq!(
            u,
            RefUpdate {
                name: REF_STORE_FORMAT.into(),
                value: encode_u32(STORE_FORMAT),
                at: log_end,
            }
        );
        assert_eq!(tail[1].0, kinds::BATCH_COMMIT);
    }
    // every object of the fixture is in the loaded store under its own hash and kind
    for b in batches {
        for f in b.frames {
            assert!(
                r.store().has_kind(f.kind(), f.hash()).unwrap(),
                "kind {} {} is gone",
                f.kind(),
                f.hash()
            );
        }
    }
    // the reindex check opens the project itself, so the store lock is released first
    drop(reader);
    r.close().unwrap();
    rdy_qa::reindex_check::reindex_reproduces(&dir).unwrap_or_else(|v| panic!("{v:#?}"));
}

#[test]
fn ec13_the_v1_s12_fixture_covers_every_kind_and_ref_family() {
    let expect = expect();
    for k in every_kind::V12_KINDS {
        assert!(
            expect["objects"][k.to_string()].as_u64().unwrap_or(0) >= 1,
            "kind {k}"
        );
    }
    for f in V12_FAMILIES {
        assert!(
            expect["refs"]
                .as_object()
                .unwrap()
                .keys()
                .any(|n| n.starts_with(f)),
            "family {f}"
        );
    }
}

#[test]
fn ec13_the_every_kind_project_is_a_clone_with_an_origin() {
    let dir = every_kind::project();
    let r = Rdy::open(dir.path()).unwrap();
    let reader = r.store().reader();
    let id = reader.repo_id().unwrap().expect("a repo id");
    let o = reader
        .origin()
        .unwrap()
        .expect("the every-kind project is a clone: it records its origin");
    assert!(!o.fork, "a clone, not a fork: {o:?}");
    assert_eq!(o.repo_id, id, "a clone keeps its source's repo id");
    let st = r.read();
    assert_eq!(
        o.tag.as_deref(),
        Some("v0.2.0"),
        "the source's newest version"
    );
    assert_eq!(
        o.tag,
        st.versions().unwrap().last().map(|v| v.name.clone()),
        "the clone carries the same versions"
    );
    assert!(o.landing.is_some(), "the source's canon head");
    assert_eq!(
        o.landing,
        st.canon_head(),
        "the clone's canon head is the source's"
    );
    drop(st);
    // the log ends with the origin batch: one `meta/origin` update and its marker
    let mut last = vec![];
    reader
        .for_each_batch(r.store().log_end(), |b| {
            last = b
                .frames
                .iter()
                .map(|f| {
                    let name = (f.kind() == kinds::REF_UPDATE).then(|| {
                        postcard::from_bytes::<RefUpdate>(f.present_payload().unwrap())
                            .unwrap()
                            .name
                    });
                    (f.kind(), name)
                })
                .collect();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        last,
        vec![(kinds::REF_UPDATE, Some(REF_ORIGIN.to_string()))],
        "the last batch is the origin batch"
    );
    reader.check_meta().unwrap();
    drop(reader);
    // the clone carries `.rdyignore`; `build` writes `.rdy/local.yaml` into the clone
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".rdyignore")).unwrap(),
        "*.draft.yaml\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".rdy/local.yaml")).unwrap(),
        "actor: human:qa\n"
    );
    let f = every_kind::facts(&r);
    assert_eq!(f["fsck"]["unlogged_moves"], 0, "{f}");
    r.close().unwrap();
}
