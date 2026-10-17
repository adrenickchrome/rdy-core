//! dl8: the dump promise (D-69AY3). Dump format 1 with store format 10 is released with this
//! file. Every later rdy loads it with every hash kept. Never rewrite the fixture: a new format
//! adds its own fixture beside it (F2).
//!
//! The fixture's log spans segments: it was written from the every-kind project on a store whose
//! segments hold `FIXTURE_SEGMENT_MAX` bytes. So it rolls more than once and holds a batch past
//! the soft limit, and the load must put each batch at its source place.
//!
//! A later store format may add kinds and refs, never re-encode (D-F0904). A load of this fixture
//! then appends one batch after the fixture's log: it sets `meta/store_format` to that rdy's
//! store format (FW-UP1, in its smallest form). The load test allows exactly that batch.
use rdy_engine::{Rdy, load::load};
use rdy_qa::every_kind;
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader};
use rdy_store::format::{REF_STORE_FORMAT, STORE_FORMAT, encode_u32};
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::meta::REF_REPO_ID;
use rdy_store::segment::Location;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The fixture's blake3, set when it was written. A change here needs a founder decision.
const FIXTURE_BLAKE3: &str = "1aeddacd0344896487769b3398e43b301d0d2817ce7000684d2b62c4d4d7baf2";
/// The blake3 of the expect file's bytes, set when it was written. A change here needs a founder
/// decision.
const EXPECT_BLAKE3: &str = "33adfacbc275c65fdaaafb74ae67e7e98eb541578555ecdb287245f48190d7c6";
/// The ref families of the v1 fixture (store format 10): the ten of an M2 store, `canon` and the
/// open proposals (`proposal/`). Frozen with the v1 fixture: a later store format that adds a
/// family adds it to `every_kind::FAMILIES`, never here.
const V1_FAMILIES: [&str; 12] = [
    "meta/store_format",
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
];
/// The segment size limit of the store the fixture was written from.
const FIXTURE_SEGMENT_MAX: u64 = 4096;
/// The bytes in front of each frame's payload in a segment: length, kind and hash.
const FRAME_HEADER: u64 = 4 + 1 + 32;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name)
}

fn expect() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(fixture("v1-s10.expect.json")).unwrap()).unwrap()
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
/// `cargo test -p rdy-qa --release --test io dl8_write_the_v1_fixture -- --ignored`.
#[test]
#[ignore]
fn dl8_write_the_v1_fixture() {
    let (dump, expect) = (fixture("v1-s10.rdydump"), fixture("v1-s10.expect.json"));
    assert!(
        !dump.exists() && !expect.exists(),
        "the v1 fixture is released: never rewrite it"
    );
    std::fs::create_dir_all(dump.parent().unwrap()).unwrap();
    let src = every_kind::rolling_project(FIXTURE_SEGMENT_MAX);
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
fn dl8_the_v1_fixture_is_never_rewritten() {
    // A change to either file needs a founder decision.
    let bytes = std::fs::read(fixture("v1-s10.rdydump")).unwrap();
    assert_eq!(rdy_model::Hash::of(&bytes).to_string(), FIXTURE_BLAKE3);
    let bytes = std::fs::read(fixture("v1-s10.expect.json")).unwrap();
    assert_eq!(rdy_model::Hash::of(&bytes).to_string(), EXPECT_BLAKE3);
}

#[test]
fn dl8_the_v1_fixture_loads_with_every_hash() {
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    let s = load(&fixture("v1-s10.rdydump"), &dir).unwrap();
    assert_eq!(s.dump_format, 1);
    let expect = expect();
    let (batches, log_end, fixture_format) = dump_batches(&fixture("v1-s10.rdydump"));
    let r = Rdy::open(&dir).unwrap();
    let got = every_kind::facts(&r);
    assert_eq!(
        got["landings"], expect["landings"],
        "every landing, in order"
    );
    // every ref holds its value, and no other ref exists; `meta/store_format` names this rdy's
    // store format (the upgrade batch sets it when the fixture's format is older)
    let mut want = expect["refs"].clone();
    want[REF_STORE_FORMAT] = json!(hex(&encode_u32(STORE_FORMAT)));
    // RULING F2 (Export and clone): a log with no repo id gets one in the upgrade batch.
    let repo_id = got["refs"][REF_REPO_ID]
        .as_str()
        .expect("a repo id")
        .to_string();
    assert_eq!(repo_id.len(), 32, "a repo id is 16 bytes: {repo_id}");
    want[REF_REPO_ID] = json!(repo_id);
    assert_eq!(
        got["refs"], want,
        "every ref: views, tags, proposals, inboxes, reviews"
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
    // past the fixture's log end: only the upgrade batch, and only for a newer store format
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
        assert_eq!(tail.len(), 3, "two ref frames and their marker: {tail:?}");
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
        assert_eq!(tail[1].0, kinds::REF_UPDATE);
        let u: RefUpdate = postcard::from_bytes(&tail[1].1).unwrap();
        assert_eq!(
            (hex(&u.value), u.at),
            (
                repo_id,
                Location {
                    segment: log_end.segment,
                    offset: log_end.offset + 1,
                }
            )
        );
        assert_eq!(u.name, REF_REPO_ID);
        assert_eq!(tail[2].0, kinds::BATCH_COMMIT);
    }
    // every object of the fixture is in the loaded store under its own hash and kind
    for b in batches {
        for f in b.frames {
            assert!(
                r.store().has_kind(f.kind, &f.hash).unwrap(),
                "kind {} {} is gone",
                f.kind,
                f.hash
            );
        }
    }
    // the reindex check opens the project itself, so the store lock is released first
    drop(reader);
    r.close().unwrap();
    rdy_qa::reindex_check::reindex_reproduces(&dir).unwrap_or_else(|v| panic!("{v:#?}"));
}

#[test]
fn dl8_the_v1_fixture_covers_every_kind_and_ref_family() {
    let expect = expect();
    for k in every_kind::V1_KINDS {
        assert!(
            expect["objects"][k.to_string()].as_u64().unwrap_or(0) >= 1,
            "kind {k}"
        );
    }
    for f in V1_FAMILIES {
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
fn dl8_the_v1_fixture_spans_segments_and_holds_a_batch_past_the_limit() {
    let expect = expect();
    let (batches, _, _) = dump_batches(&fixture("v1-s10.rdydump"));
    assert_eq!(
        starts(batches.iter().map(|b| b.start)),
        expect["batch_starts"],
        "the expect file records each batch's start"
    );
    let segments: BTreeSet<u32> = batches.iter().map(|b| b.start.segment).collect();
    assert!(
        segments.len() >= 3,
        "the log rolls at least twice: {segments:?}"
    );
    // a batch past the soft limit: its data frames alone end past the segment limit
    let max = expect["segment_max"].as_u64().unwrap();
    assert!(
        batches.iter().any(|b| {
            let len: u64 = b
                .frames
                .iter()
                .map(|f| FRAME_HEADER + f.payload.len() as u64)
                .sum();
            b.start.offset + len > max
        }),
        "a batch passes the limit of {max} bytes"
    );
}
