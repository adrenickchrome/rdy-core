//! di2: the dump promise for store format 11 (D-69AY3, D-FR0C4). The quality gate keeps one
//! fixture for each pair of dump format and store format: `(1, 11)` is released at the merge of
//! Decision intake, with this file. Every later rdy loads it with every hash kept. Write it once:
//! until the merge only the orchestrator writes it again (O29); from the merge on, never.
//!
//! The fixture holds an `ANSWER` of each family the store format adds: `answer/` (a relayed tag
//! and a local confirm of an exception) and `pending/` (a relayed snooze of an approve item). As
//! the v1-s10 fixture, it was written from the every-kind project on a store whose segments hold
//! `FIXTURE_SEGMENT_MAX` bytes, so the load must put each batch at its source place.
//!
//! A later store format may add kinds and refs, never re-encode (D-F0904). A load of this fixture
//! then appends one batch after the fixture's log: it sets `meta/store_format` to that rdy's
//! store format (FW-UP1). The load test allows exactly that batch.
use rdy_engine::{Rdy, load::load};
use rdy_model::{AnswerAct, EvidenceKind};
use rdy_qa::every_kind;
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader};
use rdy_store::format::{
    DumpStore, LAST_SCALAR_STORE_FORMAT, REF_STORE_FEATURES, baseline_features,
};
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::meta::REF_REPO_ID;
use rdy_store::segment::Location;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The fixture's blake3, set when it was written. A change here needs a founder decision (from
/// the merge on; before it, O29).
const FIXTURE_S11_BLAKE3: &str = "9c1b407ccc5839a231b713a2f9594323a8af62041372f30410ff966ebd0dd7de";
/// The blake3 of the expect file's bytes, set when it was written. A change here needs a founder
/// decision (from the merge on; before it, O29).
const EXPECT_S11_BLAKE3: &str = "f5442712301e6b03e034ca85bd4cffb164e249bfb7b984c5acd388a12287396c";
/// The ref families of the v1-s11 fixture (store format 11): the twelve of the v1-s10 fixture,
/// `answer/` and `pending/`. Frozen with this fixture: a later store format that adds a family
/// adds it to `every_kind::FAMILIES`, never here.
const V11_FAMILIES: [&str; 14] = [
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
    "answer/",
    "pending/",
];
/// The segment size limit of the store the fixture was written from.
const FIXTURE_SEGMENT_MAX: u64 = 4096;
const DUMP: &str = "v1-s11.rdydump";
const EXPECT: &str = "v1-s11.expect.json";

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
    (out, h.log_end, legacy(&h.store))
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
/// `cargo test -p rdy-qa --release --test io di2_write_the_v1_s11_fixture -- --ignored`.
#[test]
#[ignore]
fn di2_write_the_v1_s11_fixture() {
    let (dump, expect) = (fixture(DUMP), fixture(EXPECT));
    assert!(
        !dump.exists() && !expect.exists(),
        "the v1-s11 fixture is written once: never rewrite it (O29)"
    );
    assert_eq!(
        LAST_SCALAR_STORE_FORMAT, 11,
        "the v1-s11 fixture holds store format 11"
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
fn di2_the_v1_s11_fixture_is_never_rewritten() {
    // A change to either file needs a founder decision (from the merge on; before it, O29).
    let bytes = std::fs::read(fixture(DUMP)).unwrap();
    assert_eq!(rdy_model::Hash::of(&bytes).to_string(), FIXTURE_S11_BLAKE3);
    let bytes = std::fs::read(fixture(EXPECT)).unwrap();
    assert_eq!(rdy_model::Hash::of(&bytes).to_string(), EXPECT_S11_BLAKE3);
}

#[test]
fn di2_the_v1_s11_fixture_loads_with_every_hash() {
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    let s = load(&fixture(DUMP), &dir).unwrap();
    assert_eq!(s.dump_format, 1);
    assert_eq!(
        s.store_format,
        DumpStore::Legacy(11),
        "the fixture's store format"
    );
    let expect = expect();
    let (batches, log_end, fixture_format) = dump_batches(&fixture(DUMP));
    assert_eq!(fixture_format, 11);
    let r = Rdy::open(&dir).unwrap();
    let got = every_kind::facts(&r);
    assert_eq!(
        got["landings"], expect["landings"],
        "every landing, in order"
    );
    // every ref holds its value, and no other ref exists; `meta/store_format` names this rdy's
    // store format (the upgrade batch sets it when the fixture's format is older)
    let mut want = expect["refs"].clone();
    want[REF_STORE_FEATURES] = json!(hex(&baseline_features().to_json()));
    // RULING F2 (Export and clone): a log with no repo id gets one in the upgrade batch.
    let repo_id = got["refs"][REF_REPO_ID]
        .as_str()
        .expect("a repo id")
        .to_string();
    assert_eq!(repo_id.len(), 32, "a repo id is 16 bytes: {repo_id}");
    want[REF_REPO_ID] = json!(repo_id);
    assert_eq!(
        got["refs"], want,
        "every ref: views, tags, proposals, inboxes, reviews, answers, snoozes"
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
    // past the fixture's log end: nothing while the store format is 11; only the upgrade batch
    // for a newer store format
    let mut tail = vec![];
    reader
        .for_each_frame_from(log_end, r.store().log_end(), |_, kind, _, payload, _| {
            tail.push((kind, payload.to_vec()));
            Ok(())
        })
        .unwrap();
    if fixture_format > LAST_SCALAR_STORE_FORMAT {
        assert!(tail.is_empty(), "nothing past the fixture's log: {tail:?}");
    } else {
        assert_eq!(tail.len(), 3, "two ref frames and their marker: {tail:?}");
        assert_eq!(tail[0].0, kinds::REF_UPDATE);
        let u: RefUpdate = postcard::from_bytes(&tail[0].1).unwrap();
        assert_eq!(
            u,
            RefUpdate {
                name: REF_STORE_FEATURES.into(),
                value: baseline_features().to_json(),
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
    // the first writable start logs the moves content-2 leaves, then the logged invariant holds
    crate::io_dump_fixture_s15::first_start_logs_moves(&dir, false, "v1-s11");
    rdy_qa::reindex_check::reindex_reproduces(&dir).unwrap_or_else(|v| panic!("{v:#?}"));
}

#[test]
fn di2_the_v1_s11_fixture_covers_every_kind_and_ref_family() {
    let expect = expect();
    for k in every_kind::V11_KINDS {
        assert!(
            expect["objects"][k.to_string()].as_u64().unwrap_or(0) >= 1,
            "kind {k}"
        );
    }
    for f in V11_FAMILIES {
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
fn di2_the_every_kind_project_writes_an_answer_of_each_family() {
    let dir = every_kind::project();
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let reader = st.reader();
    let mut acts = vec![];
    for family in ["answer/", "pending/"] {
        let found = reader.answers_with_prefix(family).unwrap();
        assert!(!found.is_empty(), "no answer under {family}");
        for (name, _, a) in found {
            acts.push((
                family,
                a.act,
                a.person.actor.clone(),
                a.relayed_by.is_some(),
                a.evidence_kind,
                name,
            ));
        }
    }
    let has = |family: &str, act: AnswerAct, relayed: bool, kind: Option<EvidenceKind>| {
        acts.iter().any(|(f, a, person, by, k, _)| {
            *f == family && *a == act && person == "human:qa" && *by == relayed && *k == kind
        })
    };
    // a relayed tag: the TAG and an ANSWER that holds the relay (D-DGCH7)
    assert!(
        has("answer/", AnswerAct::Tag, true, Some(EvidenceKind::Text)),
        "a relayed tag: {acts:#?}"
    );
    // a local confirm of an exception
    assert!(
        has("answer/", AnswerAct::Confirm, false, None),
        "a local confirm: {acts:#?}"
    );
    // a relayed snooze of an approve item, under the person
    assert!(
        has("pending/", AnswerAct::Later, true, Some(EvidenceKind::Card)),
        "a relayed snooze: {acts:#?}"
    );
    assert!(
        acts.iter().any(|(f, a, _, _, _, name)| *f == "pending/"
            && *a == AnswerAct::Later
            && name.starts_with("pending/human:qa/P-")),
        "the snooze is of an approve item: {acts:#?}"
    );
    drop(st);
    let f = every_kind::facts(&r);
    assert!(
        f["objects"][kinds::ANSWER.to_string()]
            .as_u64()
            .unwrap_or(0)
            >= 3,
        "an ANSWER frame for each answer: {f}"
    );
    r.close().unwrap();
}

/// The scalar store format of a released dump's header.
fn legacy(s: &DumpStore) -> u32 {
    match s {
        DumpStore::Legacy(n) => *n,
        DumpStore::Features(_) => panic!("a released pair holds a scalar store"),
    }
}
