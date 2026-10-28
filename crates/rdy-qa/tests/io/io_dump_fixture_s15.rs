//! ha17: the dump promise for store format 15 (D-69AY3: "every later rdy reads every dump format
//! that rdy has released"; D-FR0C4: a fixture for each pair; D-WWAK7; D-F0904: a later store
//! format adds kinds and refs, never re-encodes). The pair `(2, 15)` is released with two
//! fixtures and this file. Every later rdy loads both with every hash kept. Write them once:
//! never rewrite them, and never regenerate them to fix a later reader.
//!
//! - `v2-s15` is a dump of the every-kind project (`rdy_qa::every_kind::rolling_project`), as
//!   the earlier pairs: every kind of store format 15, every project ref family, a clone's
//!   origin, real dropped records and the QA signer's three signatures.
//! - `v2-s15-holder` is a dump of the every-kind holder (`rdy_qa::every_kind::holder`), filled
//!   by real exports, batch checks and all-files takes: the holder mode, no canon, three tags
//!   whose histories diverge, a relay answer's `answer/tag-object:<hash>` ref, and a removal
//!   approved in one source that erased the copies both histories held. Its drops are
//!   authorized by logical proof alone, which a load and an open derive again from the log.
//!
//! Both were written on stores whose segments hold `FIXTURE_SEGMENT_MAX` bytes, so a load must
//! put each batch at its source place. A later store format may add kinds and refs, never
//! re-encode (D-F0904): a load of these fixtures then appends one batch after the fixture's log,
//! which sets `meta/store_features` to the baseline features (FW-UP1). The tests allow exactly
//! that batch. No adapter is used here: the engine and the store are the dump's readers.
use rdy_engine::{Rdy, Scope, load::load};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::{BlockId, Hash};
use rdy_qa::every_kind;
use rdy_store::dump::{DUMP_FORMAT, DumpBatch, DumpItem, DumpReader, Frame, READS};
use rdy_store::format::{DumpStore, REF_STORE_FEATURES, REF_STORE_FORMAT, baseline_features};
use rdy_store::holder::{ANSWER_TAG_OBJECT_REF, REF_STORE_MODE};
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::meta::{REF_ORIGIN, REF_REPO_ID};
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The project fixture's blake3, set when it was written. A change here needs a founder
/// decision.
const FIXTURE_BLAKE3: &str = "850a1cb32305a7dcf2e4c1e9e6f5c4c248341b1eb35ff08a8d6cfd4bf20ae344";
/// The blake3 of the project fixture's expect file. A change here needs a founder decision.
const EXPECT_BLAKE3: &str = "33770129ca00abb1b64db455bc40b7e64577d790136ff07f4c9dd5077666721d";
/// The holder fixture's blake3, set when it was written. A change here needs a founder decision.
const HOLDER_BLAKE3: &str = "072a95aaab7bcbb86383768bbc9f21c0f2ec135e8955fe72e595c4d3b9cf3592";
/// The blake3 of the holder fixture's expect file. A change here needs a founder decision.
const HOLDER_EXPECT_BLAKE3: &str =
    "70ad6716969788f6fddc15f5cab4819134c76519fbb13c4e8e53e3d31adfa20c";

/// The ref families of the v2-s15 project fixture: the sixteen of the v2-s14 fixture. Store
/// format 15 adds no project family. Frozen with this fixture.
const V15_FAMILIES: [&str; 16] = [
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
/// The ref families of the v2-s15 holder fixture, which are all a holder may hold but
/// `meta/origin` (a holder made by its creation has none): store format 15's mode and the relay
/// answer's ref by tag object. Frozen with this fixture.
const HOLDER_FAMILIES: [&str; 5] = [
    "meta/store_format",
    "meta/repo_id",
    "meta/store_mode",
    "tag/",
    "answer/tag-object:",
];
/// The object kinds the holder fixture holds: the portable kinds a tag history carries, and the
/// ref updates. No view op, intent, alternative set, review, tension or transition reaches a
/// holder. Frozen with this fixture.
const HOLDER_KINDS: [u8; 14] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::REF_UPDATE,
    kinds::TAG,
    kinds::ANSWER,
    kinds::SIGNATURE,
    kinds::REDACTION,
    kinds::REDACTION_RESIDUE,
    kinds::REDACTION_APPLY,
];
/// The segment size limit of the stores the fixtures were written from.
const FIXTURE_SEGMENT_MAX: u64 = 4096;
/// The id seed of the project fixture's writer.
const FIXTURE_ID_SEED: u64 = 15;
/// The id seed of the holder fixture's writer: another stream, so the holder's repo id is not
/// the project's.
const HOLDER_ID_SEED: u64 = 0x1515;
const DUMP: &str = "v2-s15.rdydump";
const EXPECT: &str = "v2-s15.expect.json";
pub(crate) const HOLDER_DUMP: &str = "v2-s15-holder.rdydump";
const HOLDER_EXPECT: &str = "v2-s15-holder.expect.json";

pub(crate) fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name)
}

fn read_expect(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The dump at `p`: its batches in order, the source log end its header names, its dump format
/// and its store format.
fn dump_batches(p: &Path) -> (Vec<DumpBatch>, Location, u32, u32) {
    let (mut rd, format, h) = DumpReader::open(std::fs::File::open(p).unwrap()).unwrap();
    let mut out = vec![];
    while let Some(item) = rd.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            out.push(b);
        }
    }
    let store = match h.store {
        DumpStore::Legacy(n) => n,
        DumpStore::Features(_) => panic!("{}: a released pair holds a scalar store", p.display()),
    };
    (out, h.log_end, format, store)
}

/// Each batch's start as `[segment, offset]`, in log order.
fn starts(bs: impl IntoIterator<Item = Location>) -> Value {
    json!(
        bs.into_iter()
            .map(|l| [u64::from(l.segment), l.offset])
            .collect::<Vec<_>>()
    )
}

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

/// What the one applied removal left, as the store reads it: the request, its apply and report
/// hash, the manifest's selected versions and blobs, every dropped frame of the log with its
/// place, original kind, hash and length and the apply and residue that authorize its absence,
/// the stub each removed version reads as, the manifest's inside rows, and every tag's landing.
fn removal_facts(r: &Rdy) -> Value {
    let rm = every_kind::removal(r);
    let st = r.read();
    let reader = st.reader();
    let mut dropped = vec![];
    reader
        .for_each_record_from(ORIGIN, st.seq(), |loc, f, _| {
            if let StoredFrame::Dropped {
                kind,
                hash,
                len,
                apply,
                residue,
            } = f
            {
                dropped.push(json!({
                    "at": [loc.segment, loc.offset],
                    "kind": kind,
                    "hash": hash.to_string(),
                    "len": len,
                    "apply": apply.to_string(),
                    "residue": residue.map(|h| h.to_string()),
                }));
            }
            Ok(())
        })
        .unwrap();
    let mut stubs = BTreeMap::new();
    for (id, v) in &rm.record.manifest.versions {
        let b = reader.get_block(v).unwrap();
        let ident = b
            .read_identity
            .clone()
            .expect("a removed version reads as its stub");
        stubs.insert(
            format!("{id}@{v}"),
            json!({
                "id": b.id.to_string(),
                "block_type": b.block_type,
                "parent": b.parent.as_ref().map(BlockId::to_string),
                "body": b.body,
                "fields": b.fields.len(),
                "links": b.links.len(),
                "version": ident.version.to_string(),
                "content_hash": ident.content_hash.to_string(),
                "request": ident.request.to_string(),
            }),
        );
    }
    let tags: BTreeMap<String, String> = st
        .versions()
        .unwrap()
        .into_iter()
        .map(|v| (v.name.clone(), v.landing.to_string()))
        .collect();
    let m = &rm.record.manifest;
    json!({
        "request": rm.request.to_string(),
        "apply": rm.apply.to_string(),
        "carrier": rm.record.carrier.to_string(),
        "report_hash": rm.applied.report_hash.to_string(),
        "landing": rm.applied.landing.to_string(),
        "targets": serde_json::to_value(&rm.record.targets).unwrap(),
        "versions": m.versions.iter().map(|(id, v)| format!("{id}@{v}")).collect::<Vec<_>>(),
        "blobs": m.blobs.iter().map(Hash::to_string).collect::<Vec<_>>(),
        "shared_blobs": m.shared_blobs.iter().map(Hash::to_string).collect::<Vec<_>>(),
        "dropped": dropped,
        "stubs": stubs,
        "inside": serde_json::to_value(&m.inside).unwrap(),
        "tags": tags,
    })
}

/// Creates `p` and writes `bytes` to it; refuses when `p` exists.
fn write_new(p: &Path, bytes: &[u8]) {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .unwrap_or_else(|e| panic!("{}: never rewrite a fixture member: {e}", p.display()));
    f.write_all(bytes).unwrap();
    f.sync_all().unwrap();
}

/// The dump of `r` (a project, or a holder when `holder`) at `tmp`, and what a load must keep
/// of it: the facts, the identity, the removal, and for a holder each tag's projection, the
/// segment limit and each batch's start.
fn dump_and_facts(r: &Rdy, tmp: &Path, holder: bool) -> Value {
    let d = r.read().dump(tmp).unwrap();
    assert_eq!(
        (d.dump_format, &d.store_format),
        (2, &DumpStore::Legacy(15))
    );
    let (batches, log_end, ..) = dump_batches(tmp);
    let mut facts = every_kind::facts(r);
    facts["identity"] = every_kind::identity_facts(r, Some(log_end));
    facts["redaction"] = removal_facts(r);
    if holder {
        facts["projections"] = every_kind::projections(r);
    }
    facts["segment_max"] = json!(FIXTURE_SEGMENT_MAX);
    facts["batch_starts"] = starts(batches.iter().map(|b| b.start));
    facts
}

/// Writes the four dump fixture members and the two sync members once. Run by hand, never by
/// the gate:
/// `cargo test -p rdy-qa --release --test io ha17_write_fixtures_once -- --ignored`.
#[test]
#[ignore]
fn ha17_write_fixtures_once() {
    use crate::io_sync_fixture::{SYNC, SYNC_EXPECT, SYNC_TAG, stream_facts, sync_fixture};
    let members = [
        fixture(DUMP),
        fixture(EXPECT),
        fixture(HOLDER_DUMP),
        fixture(HOLDER_EXPECT),
        sync_fixture(SYNC),
        sync_fixture(SYNC_EXPECT),
    ];
    for m in &members {
        assert!(
            !m.exists(),
            "{}: the v2-s15 fixtures are written once: never rewrite them",
            m.display()
        );
    }
    assert_eq!(
        DUMP_FORMAT, 2,
        "the v2-s15 fixtures hold dump format 2 and store format 15: this rdy writes another \
         format, and a released set is never regenerated"
    );
    std::fs::create_dir_all(sync_fixture(SYNC).parent().unwrap()).unwrap();
    // members go to a scratch directory first, so each one is only ever created whole
    let scratch = tempfile::tempdir().unwrap();

    rdy_model::seed_ids_for_tests(FIXTURE_ID_SEED);
    let src = every_kind::rolling_project(FIXTURE_SEGMENT_MAX);
    rdy_model::unseed_ids_for_tests();
    let r = Rdy::open(src.path()).unwrap();
    let tmp = scratch.path().join(DUMP);
    let facts = dump_and_facts(&r, &tmp, false);
    r.close().unwrap();
    let project = (
        std::fs::read(&tmp).unwrap(),
        serde_json::to_string_pretty(&facts).unwrap() + "\n",
    );

    rdy_model::seed_ids_for_tests(HOLDER_ID_SEED);
    let built = every_kind::holder(FIXTURE_SEGMENT_MAX);
    rdy_model::unseed_ids_for_tests();
    let h = Rdy::open_holder(&built.root).unwrap();
    let tmp = scratch.path().join(HOLDER_DUMP);
    let facts = dump_and_facts(&h, &tmp, true);
    let holder = (
        std::fs::read(&tmp).unwrap(),
        serde_json::to_string_pretty(&facts).unwrap() + "\n",
    );
    // the sync batch: the whole history of the holder's relayed tag
    let tag = h
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == SYNC_TAG)
        .unwrap()
        .1;
    let sync = every_kind::export_tag(&h, tag);
    h.close().unwrap();
    let mut sync_facts = stream_facts(&sync);
    let (taken, _, got) = crate::io_sync_fixture::take_split(&sync, &scratch.path().join("take"));
    taken.close().unwrap();
    sync_facts["taken"] = got;
    let sync = (
        sync,
        serde_json::to_string_pretty(&sync_facts).unwrap() + "\n",
    );

    for ((dump, expect), (bytes, text)) in [
        ((DUMP, EXPECT), &project),
        ((HOLDER_DUMP, HOLDER_EXPECT), &holder),
    ] {
        write_new(&fixture(dump), bytes);
        write_new(&fixture(expect), text.as_bytes());
        eprintln!("{dump} blake3 {}", Hash::of(bytes));
        eprintln!("{expect} blake3 {}", Hash::of(text.as_bytes()));
    }
    write_new(&sync_fixture(SYNC), &sync.0);
    write_new(&sync_fixture(SYNC_EXPECT), sync.1.as_bytes());
    eprintln!("{SYNC} blake3 {}", Hash::of(&sync.0));
    eprintln!("{SYNC_EXPECT} blake3 {}", Hash::of(sync.1.as_bytes()));
}

#[test]
fn ha17_dump_fixtures_are_never_rewritten() {
    // A change to any member needs a founder decision. The checksums are fixed here, apart from
    // any writer: a later writer that writes other bytes changes nothing in this check.
    for (name, digest) in [
        (DUMP, FIXTURE_BLAKE3),
        (EXPECT, EXPECT_BLAKE3),
        (HOLDER_DUMP, HOLDER_BLAKE3),
        (HOLDER_EXPECT, HOLDER_EXPECT_BLAKE3),
    ] {
        let bytes = std::fs::read(fixture(name)).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), digest, "{name}");
    }
}

/// The records of `r`'s log from `from` to its end: each kind and payload (empty for a dropped
/// frame).
fn tail(r: &Rdy, from: Location) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    r.store()
        .reader()
        .for_each_record_from(from, r.store().log_end(), |_, f, _| {
            match f {
                StoredFrame::Present { kind, payload, .. } => out.push((kind, payload.to_vec())),
                StoredFrame::Dropped { kind, .. } => out.push((kind, vec![])),
            }
            Ok(())
        })
        .unwrap();
    out
}

/// `v` (store facts, or any part of them) without each fsck's `unlogged_moves` count: the
/// historical facts of a released fixture, which content-2 does not change (D-69AY3). The count
/// is analysis: under content-2 (D-TX3KH, D-XTPJE) a store an older rdy wrote has moves left for
/// its first writable start, which [`first_start_logs_moves`] checks apart.
pub(crate) fn without_moves(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| k.as_str() != "unlogged_moves")
                .map(|(k, x)| (k.clone(), without_moves(x)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(without_moves).collect()),
        x => x.clone(),
    }
}

/// The first writable start of the store loaded at `dir` (a holder when `holder`): it logs
/// exactly the moves fsck counts as unlogged, as one batch of one TRANSITION record whose every
/// move has cause `reopened`, and a second start logs nothing and leaves fsck with no unlogged
/// move. Returns the count. The logged invariant holds only after it (D-XTPJE: the first daemon
/// start logs one reopened batch).
pub(crate) fn first_start_logs_moves(dir: &Path, holder: bool, what: &str) -> usize {
    use rdy_store::transition::Cause;
    let open = || {
        if holder {
            Rdy::open_holder(dir).unwrap()
        } else {
            Rdy::open(dir).unwrap()
        }
    };
    let mut r = open();
    let n = r.fsck().unwrap().unlogged_moves;
    let at = r.store().log_end();
    assert_eq!(
        r.reconcile_transitions().unwrap(),
        n,
        "{what}: the first start logs every unlogged move"
    );
    if n > 0 {
        let batch = r.store().last_batch().to_vec();
        assert_eq!(batch.len(), 1, "{what}: one record in one batch: {batch:?}");
        assert_eq!(batch[0].0, kinds::TRANSITION, "{what}");
        assert!(r.store().log_end() > at, "{what}");
        let rec = r.read().reader().transitions(&batch[0].1).unwrap();
        let causes: Vec<Cause> = rec
            .opened
            .iter()
            .chain(&rec.resolved)
            .map(|m| m.why.cause)
            .chain(rec.new.iter().chain(&rec.gone).map(|m| m.why.cause))
            .collect();
        assert_eq!(causes.len(), n, "{what}: {rec:?}");
        assert!(
            causes.iter().all(|c| *c == Cause::Reopened),
            "{what}: every move is reopened: {rec:?}"
        );
    } else {
        assert_eq!(
            r.store().log_end(),
            at,
            "{what}: nothing to log, nothing written"
        );
    }
    r.close().unwrap();
    let mut r = open();
    let after = r.store().log_end();
    assert_eq!(
        r.reconcile_transitions().unwrap(),
        0,
        "{what}: a second start"
    );
    assert_eq!(
        r.store().log_end(),
        after,
        "{what}: a second start writes nothing"
    );
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0, "{what}");
    r.close().unwrap();
    n
}

/// Past a fixture's log end (a fixture of store format `fixture_format`, which holds its repo
/// id), a load holds only the upgrade batch to a feature store, which holds
/// `meta/store_features` alone: the baseline features. Returns the number of data frames past
/// the end.
pub(crate) fn upgrade_tail(r: &Rdy, log_end: Location, fixture_format: u32) -> u64 {
    let tail = tail(r, log_end);
    assert!(fixture_format >= 12, "a fixture that holds its repo id");
    assert_eq!(tail.len(), 2, "one ref frame and its marker: {tail:?}");
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
    assert_eq!(tail[1].0, kinds::BATCH_COMMIT);
    tail.iter()
        .filter(|(kind, _)| *kind != kinds::BATCH_COMMIT)
        .count() as u64
}

/// Loads the fixture `dump` and checks it against `expect`: the pair, no removed canary in its
/// bytes, every ref (with this rdy's store format), signature, landing, kind count, identity,
/// removal and batch place, the fsck counts, and each object of the dump under its own hash and
/// kind. Returns the opened store and its directory's guard.
fn load_and_check(dump: &str, expect: &Value, holder: bool) -> (Rdy, tempfile::TempDir) {
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    let s = load(&fixture(dump), &dir).unwrap();
    assert_eq!(
        (s.dump_format, &s.store_format),
        (2, &DumpStore::Legacy(15))
    );
    let (batches, log_end, format, fixture_format) = dump_batches(&fixture(dump));
    assert_eq!((format, fixture_format), (2, 15));
    let bytes = std::fs::read(fixture(dump)).unwrap();
    let canaries: [&[u8]; 4] = [
        every_kind::CANARY_BODY.as_bytes(),
        every_kind::CANARY_FIELD.as_bytes(),
        every_kind::HOLDER_CANARY.as_bytes(),
        every_kind::HOLDER_BLOB,
    ];
    for canary in canaries {
        assert!(
            !bytes.windows(canary.len()).any(|w| w == canary),
            "{dump}: a selected canary is in the fixture"
        );
    }
    let r = if holder {
        Rdy::open_holder(&dir).unwrap()
    } else {
        Rdy::open(&dir).unwrap()
    };
    let got = every_kind::facts(&r);
    assert_eq!(got["landings"], expect["landings"], "{dump}: landings");
    let mut want = expect["refs"].clone();
    // the historical store format stays; the upgrade batch adds the features
    assert_eq!(want[REF_STORE_FORMAT], json!(hex(b"15")));
    want[REF_STORE_FEATURES] = json!(hex(&baseline_features().to_json()));
    assert!(want[REF_REPO_ID].is_string(), "{dump}: a repo id");
    assert_eq!(got["refs"], want, "{dump}: every ref");
    assert_eq!(
        got["signatures"], expect["signatures"],
        "{dump}: every signature, with its check"
    );
    // the fixture's own log range: every original kind counted, dropped frames too, each key and
    // each batch at its source place
    let reader = r.store().reader();
    let mut objects: BTreeMap<String, u64> = BTreeMap::new();
    reader
        .for_each_record_from(ORIGIN, log_end, |_, f, _| {
            let kind = match f {
                StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } => {
                    assert_eq!(Hash::of(payload), hash, "a payload keeps its hash");
                    kind
                }
                StoredFrame::Dropped { kind, .. } => kind,
            };
            if kind != kinds::BATCH_COMMIT {
                *objects.entry(kind.to_string()).or_default() += 1;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(json!(objects), expect["objects"], "{dump}: every kind");
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
        "{dump}: each batch at its source place"
    );
    drop(reader);
    assert_eq!(
        every_kind::identity_facts(&r, Some(log_end)),
        expect["identity"],
        "{dump}: the identity"
    );
    let upgrade = upgrade_tail(&r, log_end, fixture_format);
    for b in &batches {
        for f in &b.frames {
            assert!(
                r.store().has_kind(f.kind(), f.hash()).unwrap(),
                "{dump}: kind {} {} is gone",
                f.kind(),
                f.hash()
            );
        }
    }
    assert_eq!(
        removal_facts(&r),
        expect["redaction"],
        "{dump}: the removal"
    );
    for (key, stub) in expect["redaction"]["stubs"].as_object().unwrap() {
        assert_eq!(stub["body"], CONTENT_REMOVED, "{key}");
        assert_eq!(
            (stub["fields"].as_u64(), stub["links"].as_u64()),
            (Some(0), Some(0))
        );
    }
    let mut want_fsck = expect["fsck"].clone();
    for member in ["frames", "checked_payloads"] {
        want_fsck[member] = json!(want_fsck[member].as_u64().unwrap() + upgrade);
    }
    // the unlogged move count is analysis, which the first writable start logs
    // (`first_start_logs_moves`)
    assert_eq!(
        without_moves(&got["fsck"]),
        without_moves(&want_fsck),
        "{dump}: fsck counts"
    );
    (r, to)
}

#[test]
fn ha17_new_pair_loads_every_kind_and_holder_ref() {
    // the project: every kind of store format 15 and every project ref family
    let expect = read_expect(EXPECT);
    let (r, to) = load_and_check(DUMP, &expect, false);
    for k in every_kind::V15_KINDS {
        assert!(
            expect["objects"][k.to_string()].as_u64().unwrap_or(0) >= 1,
            "kind {k}"
        );
    }
    assert_eq!(
        every_kind::V15_KINDS,
        every_kind::V14_KINDS,
        "store format 15 adds no kind"
    );
    assert!(
        expect["objects"].get("12").is_none(),
        "kind 12 is unassigned"
    );
    let refs = expect["refs"].as_object().unwrap();
    for f in V15_FAMILIES {
        assert!(refs.keys().any(|n| n.starts_with(f)), "family {f}");
    }
    assert!(
        !refs.contains_key(REF_STORE_MODE),
        "a project has no mode ref"
    );
    assert_eq!(expect["identity"]["mode"], "project");
    assert!(expect["identity"]["canon"].is_string());
    assert!(
        expect["identity"]["origin"].is_object(),
        "the clone's origin"
    );
    assert!(refs.contains_key(REF_ORIGIN));
    assert_eq!(expect["signatures"].as_object().unwrap().len(), 3);
    assert!(expect["redaction"]["dropped"].as_array().unwrap().len() > 1);
    // the first writable start logs the moves content-2 leaves, then the logged invariant holds
    r.close().unwrap();
    first_start_logs_moves(&to.path().join("p"), false, DUMP);
    let r = Rdy::open(&to.path().join("p")).unwrap();
    rdy_qa::invariants::check_all(&r).unwrap_or_else(|v| panic!("{v:#?}"));
    r.close().unwrap();
    rdy_qa::reindex_check::reindex_reproduces(&to.path().join("p"))
        .unwrap_or_else(|v| panic!("{v:#?}"));

    // the holder: its mode, no canon, divergent tags, the relay answer's ref and portable drops
    let expect = read_expect(HOLDER_EXPECT);
    let (h, _to) = load_and_check(HOLDER_DUMP, &expect, true);
    let refs = expect["refs"].as_object().unwrap();
    for f in HOLDER_FAMILIES {
        assert!(refs.keys().any(|n| n.starts_with(f)), "family {f}");
    }
    for n in refs.keys() {
        assert!(
            HOLDER_FAMILIES.iter().any(|f| n.starts_with(f)),
            "{n} is not a holder ref"
        );
    }
    assert_eq!(refs[REF_STORE_MODE], hex(b"holder"));
    assert_eq!(
        (
            &expect["identity"]["mode"],
            &expect["identity"]["canon"],
            &expect["identity"]["origin"]
        ),
        (&json!("holder"), &Value::Null, &Value::Null)
    );
    assert_eq!(expect["landings"], json!([]), "no canon chain");
    for k in HOLDER_KINDS {
        assert!(
            expect["objects"][k.to_string()].as_u64().unwrap_or(0) >= 1,
            "kind {k}"
        );
    }
    let held: BTreeSet<String> = expect["objects"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let want: BTreeSet<String> = HOLDER_KINDS.iter().map(u8::to_string).collect();
    assert_eq!(held, want, "the holder's kinds");
    // three tags; v2.0.0 leaves the history of v1.0.0 and v3.0.0 after the shared landing
    let tags = expect["identity"]["tags"].as_object().unwrap();
    assert_eq!(
        tags.keys().map(String::as_str).collect::<Vec<_>>(),
        every_kind::HOLDER_TAGS
    );
    let landing = |name: &str| -> Hash { tags[name]["landing"].as_str().unwrap().parse().unwrap() };
    {
        let st = h.read();
        let chain = |l: Hash| -> BTreeSet<Hash> {
            let mut out = BTreeSet::new();
            let mut cur = Some(l);
            while let Some(c) = cur {
                out.insert(c);
                cur = st.reader().landing(&c).unwrap().prev;
            }
            out
        };
        let (a, b) = (chain(landing("v3.0.0")), chain(landing("v2.0.0")));
        assert!(a.contains(&landing("v1.0.0")) && b.contains(&landing("v1.0.0")));
        assert!(!a.contains(&landing("v2.0.0")) && !b.contains(&landing("v3.0.0")));
    }
    // the relay answer's ref names the relayed tag and its ANSWER
    let relay: Vec<_> = refs
        .iter()
        .filter(|(n, _)| n.starts_with(ANSWER_TAG_OBJECT_REF))
        .collect();
    assert_eq!(relay.len(), 1, "{relay:?}");
    assert_eq!(
        relay[0].0.strip_prefix(ANSWER_TAG_OBJECT_REF).unwrap(),
        tags["v3.0.0"]["tag"]
    );
    let answer: Hash = relay[0].1.as_str().unwrap().parse().unwrap();
    assert!(h.store().has_kind(kinds::ANSWER, &answer).unwrap());
    // five signatures: three approvals by the person, the person's tag and the agent's relay
    // of the relayed tag, each checked as signed and matching in the holder
    let signed = expect["signatures"].as_object().unwrap();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for v in signed.values() {
        *by_kind
            .entry(v["check"]["kind"].as_str().unwrap().to_string())
            .or_default() += 1;
        assert_eq!(
            (&v["check"]["signed"], &v["check"]["statement"]),
            (&json!("valid"), &json!("match"))
        );
    }
    assert_eq!(
        by_kind,
        BTreeMap::from([
            ("approval".to_string(), 3),
            ("relay".to_string(), 1),
            ("tag".to_string(), 1)
        ])
    );
    // the drops: the prose's versions, the changes that embed them (B's version is kept), and
    // the blob's chunk
    let dropped: BTreeSet<u64> = expect["redaction"]["dropped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["kind"].as_u64().unwrap())
        .collect();
    assert_eq!(
        dropped,
        BTreeSet::from([
            u64::from(kinds::BLOCK),
            u64::from(kinds::CHANGE),
            u64::from(kinds::BLOB_CHUNK)
        ])
    );
    assert_eq!(
        every_kind::projections(&h),
        expect["projections"],
        "each tag reads as written"
    );
    h.fsck().unwrap();
    h.close().unwrap();
}

#[test]
fn ha17_holder_dump_rebuilds_logical_authorization_without_canon() {
    let expect = read_expect(HOLDER_EXPECT);
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("h");
    load(&fixture(HOLDER_DUMP), &dir).unwrap();
    // what the holder reads: each dropped frame's absence and its request, each tag's blocks
    let reads = |h: &Rdy| {
        let mut out = vec![];
        h.store()
            .reader()
            .for_each_record_from(ORIGIN, h.store().log_end(), |loc, f, _| {
                if let StoredFrame::Dropped {
                    kind, hash, apply, ..
                } = f
                {
                    out.push(json!({
                        "at": [loc.segment, loc.offset],
                        "kind": kind,
                        "hash": hash.to_string(),
                        "apply": apply.to_string(),
                        "request": h.store().redaction_of(kind, &hash).unwrap().map(|r| r.to_string()),
                        "readable": h.store().get_kind(kind, &hash).is_ok(),
                    }));
                }
                Ok(())
            })
            .unwrap();
        json!({
            "dropped": out,
            "projections": every_kind::projections(h),
            "identity": every_kind::identity_facts(h, None),
        })
    };
    let h = Rdy::open_holder(&dir).unwrap();
    let want = reads(&h);
    let request = &expect["redaction"]["request"];
    assert!(!want["dropped"].as_array().unwrap().is_empty());
    for d in want["dropped"].as_array().unwrap() {
        assert_eq!(&d["request"], request, "{d}");
        assert_eq!(d["readable"], false, "{d}");
    }
    assert_eq!(want["projections"], expect["projections"]);
    h.close().unwrap();
    // the derived index is deleted: the open derives every removal from the sealed log alone,
    // with no map record and no canon
    std::fs::remove_file(dir.join(".rdy/store/index.redb")).unwrap();
    let mut h = Rdy::open_holder(&dir).unwrap();
    assert_eq!(reads(&h), want, "after the index is rebuilt");
    assert_eq!(h.read().canon_head(), None);
    let mut kinds_seen = BTreeSet::new();
    h.store()
        .reader()
        .for_each_record_from(ORIGIN, h.store().log_end(), |_, f, _| {
            match f {
                StoredFrame::Present { kind, .. } | StoredFrame::Dropped { kind, .. } => {
                    kinds_seen.insert(kind)
                }
            };
            Ok(())
        })
        .unwrap();
    assert!(!kinds_seen.contains(&12), "no record of kind 12");
    h.reindex().unwrap();
    assert_eq!(reads(&h), want, "after a reindex");
    // the removed prose reads as its stub at v1.0.0; B's own version of the block reads in full
    // at v2.0.0
    let st = h.read();
    let tags = expect["identity"]["tags"].as_object().unwrap();
    let at = |name: &str| Scope::At(tags[name]["landing"].as_str().unwrap().parse().unwrap());
    let kept: Vec<_> = st
        .materialized(&at("v2.0.0"))
        .unwrap()
        .versions()
        .map(|(id, _)| id.clone())
        .filter(|id| st.block(&at("v2.0.0"), id).unwrap().body == every_kind::HOLDER_KEPT_BODY)
        .collect();
    assert_eq!(kept.len(), 1, "B's version of the prose");
    let stub = st.block(&at("v1.0.0"), &kept[0]).unwrap();
    assert!(stub.is_redacted());
    assert_eq!(stub.body, CONTENT_REMOVED);
    assert!(st.block(&at("v3.0.0"), &kept[0]).unwrap().is_redacted());
    assert!(!st.block(&at("v2.0.0"), &kept[0]).unwrap().is_redacted());
    drop(st);
    h.fsck().unwrap();
    h.close().unwrap();
}

/// A released fixture: its dump, its expect file, the checksums of both and its pair.
pub(crate) struct Released {
    pub(crate) dump: &'static str,
    pub(crate) expect: &'static str,
    pub(crate) dump_blake3: &'static str,
    pub(crate) expect_blake3: &'static str,
    pub(crate) pair: (u32, u32),
    pub(crate) holder: bool,
}

/// Every released fixture of a full dump. The checksums repeat the ones each pair's own test pins:
/// a change to any file needs a founder decision, and none is made here.
pub(crate) const RELEASED: [Released; 13] = [
    Released {
        dump: "v1-s10.rdydump",
        expect: "v1-s10.expect.json",
        dump_blake3: "1aeddacd0344896487769b3398e43b301d0d2817ce7000684d2b62c4d4d7baf2",
        expect_blake3: "33adfacbc275c65fdaaafb74ae67e7e98eb541578555ecdb287245f48190d7c6",
        pair: (1, 10),
        holder: false,
    },
    Released {
        dump: "v1-s11.rdydump",
        expect: "v1-s11.expect.json",
        dump_blake3: "9c1b407ccc5839a231b713a2f9594323a8af62041372f30410ff966ebd0dd7de",
        expect_blake3: "f5442712301e6b03e034ca85bd4cffb164e249bfb7b984c5acd388a12287396c",
        pair: (1, 11),
        holder: false,
    },
    Released {
        dump: "v1-s12.rdydump",
        expect: "v1-s12.expect.json",
        dump_blake3: "c41ca2a0206997886d1f8f6439a4d2c741a472d41ae63edbae82ad13d869d9b7",
        expect_blake3: "aa203fb8f0b109f27726ddfe15fe91182f969747d8bf38c5a01692b2a4804de0",
        pair: (1, 12),
        holder: false,
    },
    Released {
        dump: "v2-s13.rdydump",
        expect: "v2-s13.expect.json",
        dump_blake3: "bf177175d1f8fc964287579a00739a20fd90bc4d78184270436cc88409292be2",
        expect_blake3: "05e4c3be472db581143745df737b8022dd4e010783641b9dc5c194869d4e5f0a",
        pair: (2, 13),
        holder: false,
    },
    Released {
        dump: "v2-s14.rdydump",
        expect: "v2-s14.expect.json",
        dump_blake3: "113ead218bba16c1deb36be43e2bee464d9bfdc278f99049f50ec0dc91bac7b1",
        expect_blake3: "802942c851e10858c70410ea28f83875f0f664cf067e10b8fa34d61b22355d33",
        pair: (2, 14),
        holder: false,
    },
    Released {
        dump: DUMP,
        expect: EXPECT,
        dump_blake3: FIXTURE_BLAKE3,
        expect_blake3: EXPECT_BLAKE3,
        pair: (2, 15),
        holder: false,
    },
    Released {
        dump: HOLDER_DUMP,
        expect: HOLDER_EXPECT,
        dump_blake3: HOLDER_BLAKE3,
        expect_blake3: HOLDER_EXPECT_BLAKE3,
        pair: (2, 15),
        holder: true,
    },
    Released {
        dump: "v3-s16.rdydump",
        expect: "v3-s16.expect.json",
        dump_blake3: "5de4badbd1e5692e12b8465f06fce5f6cea1f42fd619a18cde756584c792c2b5",
        expect_blake3: "0df206615c985815d676a5ca91efd865899209464f06f575ea235976425e6232",
        pair: (3, 16),
        holder: false,
    },
    Released {
        dump: "v3-s16-holder.rdydump",
        expect: "v3-s16-holder.expect.json",
        dump_blake3: "dc4ca5b0fcce7e097db9ecf64196ce00a26ad6c0233df11341a578f8d9e7f26e",
        expect_blake3: "c188d8ff41afdb9ecd2e6a172a20e285aef89826dcf3442bab9730ce446b1372",
        pair: (3, 16),
        holder: true,
    },
    Released {
        dump: "v3-s17.rdydump",
        expect: "v3-s17.expect.json",
        dump_blake3: "94157e0d99fbd0170ea7eeca37315e141a4776485923bd27264bbc00e70442b5",
        expect_blake3: "397680b93467ef7a9dce3fdb0e16cb1a71d8bbcdbe734ce96907d657578fd44a",
        pair: (3, 17),
        holder: false,
    },
    Released {
        dump: "v3-s17-holder.rdydump",
        expect: "v3-s17-holder.expect.json",
        dump_blake3: "004dbd2f0eb39bcf227aa4a1dfc1fe58980094bd3bbb80b52bbcb70cc8124d91",
        expect_blake3: "8c21ae51e8a2bd0689189935d56bac40506fb77890606cf70cdf0d967f39ff69",
        pair: (3, 17),
        holder: true,
    },
    Released {
        dump: "v3-s18.rdydump",
        expect: "v3-s18.expect.json",
        dump_blake3: "b69978453eaf0bc06b5e8040cd152836c1a290a913603cd53a5a2e5ece53c615",
        expect_blake3: "c2bf21e87c2c4babf30750ec8d619f204367ea8d94eeae600ff394f1429603c0",
        pair: (3, 18),
        holder: false,
    },
    Released {
        dump: "v3-s18-holder.rdydump",
        expect: "v3-s18-holder.expect.json",
        dump_blake3: "0ba16663f3cc01684e1a01e36d2941cb7ae4f92d6940be4cc3a4b833ad3afbe7",
        expect_blake3: "c6cad8a5b549df4c840ae698c8d5e6cd67830525370e76fc4f372ba638a7dccf",
        pair: (3, 18),
        holder: true,
    },
];

/// A released dump chain: its full dump and increments in chain order, its expect file, the
/// checksums of each (in that order) and its pair.
pub(crate) struct ReleasedChain {
    pub(crate) files: [&'static str; 3],
    pub(crate) expect: &'static str,
    pub(crate) blake3: [&'static str; 4],
    pub(crate) pair: (u32, u32),
    pub(crate) holder: bool,
}

/// Every released dump chain. The checksums repeat the ones `io_dump_fixture_s16`,
/// `io_dump_fixture_s17` and `io_dump_fixture_s18` pin.
pub(crate) const RELEASED_CHAINS: [ReleasedChain; 6] = [
    ReleasedChain {
        files: [
            "v3-s16-chain-full.rdydump",
            "v3-s16-chain-1.rdydump",
            "v3-s16-chain-2.rdydump",
        ],
        expect: "v3-s16-chain.expect.json",
        blake3: [
            "7deafdc1a6b2d42a5f71165ca2961d7f32c9c8f4f2a0012a1798d230d1935a62",
            "23f64d4625407f257e5a0ceb55a133c7472188f21a2ea78e291e49fd02d9069c",
            "d8735bd462d05edf2d371bed9cf56ce3ca04080fd06882b5e1a02d3e2e2b8111",
            "2b00f08d44c55b74e6e97375f52acd9e6ed9458ab15790bb31fd6d391a05b615",
        ],
        pair: (3, 16),
        holder: false,
    },
    ReleasedChain {
        files: [
            "v3-s16-holder-chain-full.rdydump",
            "v3-s16-holder-chain-1.rdydump",
            "v3-s16-holder-chain-2.rdydump",
        ],
        expect: "v3-s16-holder-chain.expect.json",
        blake3: [
            "bf1a88b6d553393f34ad812131bdcb4497347c5a1ec16273a8a8e86c2565f300",
            "f4069d58ca2dc9705f8b95cf1ae1d5795edf8af5a87ba8a30cc01289debf4262",
            "3aa9e3303fe228c02b79e3e219494992a7f4b6250e602bf0bcaea6f869150057",
            "369da6d39efc8aeb46369ed36aff7b52e5f5b20f6ba22d0c73531639159bef80",
        ],
        pair: (3, 16),
        holder: true,
    },
    ReleasedChain {
        files: [
            "v3-s17-chain-full.rdydump",
            "v3-s17-chain-1.rdydump",
            "v3-s17-chain-2.rdydump",
        ],
        expect: "v3-s17-chain.expect.json",
        blake3: [
            "c0ee2f5236c226df0eaac875b317872d10bbc2f0f441145ea9df139317636872",
            "1189a0c637444c8f9ec13ad4f6b1ee93dc12c7b2d912b4f877d253bc6e3e313d",
            "1c72ece30389ba89621cf4b01702b0a640628280d0297f3ca04418fbff73b42b",
            "9dcb2982bcae43cf79476b6ecaff2e13f528bb1a86d5f2caf0ec4e98c0f775b8",
        ],
        pair: (3, 17),
        holder: false,
    },
    ReleasedChain {
        files: [
            "v3-s17-holder-chain-full.rdydump",
            "v3-s17-holder-chain-1.rdydump",
            "v3-s17-holder-chain-2.rdydump",
        ],
        expect: "v3-s17-holder-chain.expect.json",
        blake3: [
            "ec0e4a4f6f3f5c6efde8229b8b65205adfa4a18de8098f4945864021f4ebf5eb",
            "1ff73bc53aeb46cb50f0de29997165298bcd14e14b4876bf3909db17eecd9be2",
            "bbff1ff5ce76e60d8ec2c6aacab90b9c471edf0ec4b359017dfa884c7e2bc35a",
            "b38ac9d6944e139dd4e0966c5a58648d76a937c8858910f11b92446200b55e2f",
        ],
        pair: (3, 17),
        holder: true,
    },
    ReleasedChain {
        files: [
            "v3-s18-chain-full.rdydump",
            "v3-s18-chain-1.rdydump",
            "v3-s18-chain-2.rdydump",
        ],
        expect: "v3-s18-chain.expect.json",
        blake3: [
            "7c734b3043f0b5e3873621eb97799ae7c0aab1638f1f130c9a268da9ef6e0002",
            "b1203e3fdc5bd5a8f15622d23b4cc8951bfe193ac59b23372900aecd81ceb947",
            "5837691a6d83e7537fb4e62211aaaf54f33e9948e41be0343fc23bc6f85c28e9",
            "1d93b0abc3570711b46075d5405b4df54af86fd9eeb45512ab8d511e930c7785",
        ],
        pair: (3, 18),
        holder: false,
    },
    ReleasedChain {
        files: [
            "v3-s18-holder-chain-full.rdydump",
            "v3-s18-holder-chain-1.rdydump",
            "v3-s18-holder-chain-2.rdydump",
        ],
        expect: "v3-s18-holder-chain.expect.json",
        blake3: [
            "2ac0183140a5c494bcd6d63e41eb6cc3437d684096b904d856a453be9bcfd907",
            "06a8b3bfa187101dc1179d0b9b7708e86a826068bf767bf79cf03919fd116b9e",
            "d160d815ecde5f20f4eda78cc1b0710199b56add6e5063916b61a7f25b74e974",
            "07ae3d4064f82907bbb9e4be41a062bad173d7a6dbe86265302db67607410b88",
        ],
        pair: (3, 18),
        holder: true,
    },
];

/// The blake3 of member `name` of `pair`, as that pair's own test pins it: (3, 16), (3, 17) and
/// (3, 18) each in their own module, and never another pair's name.
fn member_digest(pair: (u32, u32), name: &str) -> &'static str {
    match pair {
        (3, 16) => crate::io_dump_fixture_s16::digest_of(name),
        (3, 17) => crate::io_dump_fixture_s17::digest_of(name),
        (3, 18) => crate::io_dump_fixture_s18::digest_of(name),
        other => panic!("{name}: pair {other:?} keeps its digests here only"),
    }
}

/// Checks the store loaded at `dir` from the dump files whose batches are `batches` and whose
/// log ends at `log_end`: every original frame under its hash and kind, at most an upgrade batch
/// after the source log, and the fsck (a holder), or the invariants and a reindex (a project).
fn check_loaded(
    what: &str,
    dir: &Path,
    batches: &[DumpBatch],
    log_end: Location,
    store_format: u32,
    pair: (u32, u32),
    holder: bool,
) {
    let r = if holder {
        Rdy::open_holder(dir).unwrap()
    } else {
        Rdy::open(dir).unwrap()
    };
    let mut frames = 0;
    for b in batches {
        for fr in &b.frames {
            frames += 1;
            assert!(
                r.store().has_kind(fr.kind(), fr.hash()).unwrap(),
                "{what}: kind {} {} is gone",
                fr.kind(),
                fr.hash()
            );
        }
    }
    assert!(frames > 0, "{what}");
    assert!(
        batches
            .iter()
            .flat_map(|b| &b.frames)
            .any(|fr| matches!(fr, Frame::Dropped { .. }))
            || pair.0 == 1,
        "{what}: a dump format 2 or 3 fixture holds dropped records"
    );
    // the loaded store carries the fixture's log, then the upgrade batch of ref updates to a
    // feature store
    let tail = tail(&r, log_end);
    assert!(
        tail.iter()
            .all(|(k, _)| *k == kinds::REF_UPDATE || *k == kinds::BATCH_COMMIT),
        "{what}: the load adds ref updates only: {tail:?}"
    );
    assert!(
        !tail.is_empty() && store_format <= rdy_store::format::LAST_SCALAR_STORE_FORMAT,
        "{what}: an upgrade batch for every scalar store format"
    );
    // the unlogged moves are analysis: the first writable start logs them, then the logged
    // invariant holds
    r.close().unwrap();
    first_start_logs_moves(dir, holder, what);
    let r = if holder {
        Rdy::open_holder(dir).unwrap()
    } else {
        Rdy::open(dir).unwrap()
    };
    if holder {
        r.fsck().unwrap();
        r.close().unwrap();
    } else {
        rdy_qa::invariants::check_all(&r).unwrap_or_else(|v| panic!("{what}: {v:#?}"));
        r.close().unwrap();
        rdy_qa::reindex_check::reindex_reproduces(dir).unwrap_or_else(|v| panic!("{what}: {v:#?}"));
    }
}

#[test]
fn ha17_all_released_dump_pairs_still_load() {
    // the fixtures on disk are exactly the released ones
    let mut on_disk = BTreeSet::new();
    for e in std::fs::read_dir(fixture("")).unwrap() {
        on_disk.insert(e.unwrap().file_name().to_string_lossy().into_owned());
    }
    let listed: BTreeSet<String> = RELEASED
        .iter()
        .flat_map(|f| [f.dump.to_string(), f.expect.to_string()])
        .chain(
            RELEASED_CHAINS
                .iter()
                .flat_map(|c| c.files.iter().chain([&c.expect]).map(|n| n.to_string())),
        )
        .collect();
    let listed: BTreeSet<String> = listed
        .into_iter()
        .chain(
            crate::io_feature_fixture::dump_members()
                .into_iter()
                .map(str::to_string),
        )
        .collect();
    assert_eq!(
        on_disk, listed,
        "every fixture member is a released one or a member of a complete feature set"
    );
    // the nine released pairs, each with its fixture, and each pair this rdy reads among them
    let pairs: BTreeSet<(u32, u32)> = RELEASED.iter().map(|f| f.pair).collect();
    assert_eq!(
        pairs,
        BTreeSet::from([
            (1, 10),
            (1, 11),
            (1, 12),
            (2, 13),
            (2, 14),
            (2, 15),
            (3, 16),
            (3, 17),
            (3, 18)
        ])
    );
    assert_eq!(
        READS.iter().copied().collect::<BTreeSet<_>>(),
        pairs,
        "each released pair this rdy reads has a fixture, and each fixture's pair is read"
    );
    // the dump format this rdy writes holds a feature store; its complete set is registered
    assert_eq!(DUMP_FORMAT, 4);
    crate::io_feature_fixture::assert_written_set_is_registered();
    for c in &RELEASED_CHAINS {
        assert!(pairs.contains(&c.pair), "{}", c.expect);
    }
    for f in &RELEASED {
        // the checksum guards of each fixture are unchanged
        let bytes = std::fs::read(fixture(f.dump)).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), f.dump_blake3, "{}", f.dump);
        let bytes = std::fs::read(fixture(f.expect)).unwrap();
        assert_eq!(
            Hash::of(&bytes).to_string(),
            f.expect_blake3,
            "{}",
            f.expect
        );
        if f.pair.0 == 3 {
            for (name, digest) in [(f.dump, f.dump_blake3), (f.expect, f.expect_blake3)] {
                assert_eq!(digest, member_digest(f.pair, name), "{name}");
            }
        }
        // the dump still loads into this store format with every original object under its
        // hash and kind
        let (batches, log_end, format, store_format) = dump_batches(&fixture(f.dump));
        assert_eq!((format, store_format), f.pair, "{}", f.dump);
        let to = tempfile::tempdir().unwrap();
        let dir = to.path().join("p");
        let s = load(&fixture(f.dump), &dir).unwrap();
        assert_eq!(
            (s.dump_format, &s.store_format),
            (f.pair.0, &DumpStore::Legacy(f.pair.1)),
            "{}",
            f.dump
        );
        check_loaded(
            f.dump,
            &dir,
            &batches,
            log_end,
            store_format,
            f.pair,
            f.holder,
        );
    }
    for c in &RELEASED_CHAINS {
        // the checksum guards of each chain member are unchanged
        for (name, digest) in c.files.iter().chain([&c.expect]).zip(c.blake3) {
            let bytes = std::fs::read(fixture(name)).unwrap();
            assert_eq!(Hash::of(&bytes).to_string(), digest, "{name}");
            assert_eq!(digest, member_digest(c.pair, name), "{name}");
        }
        // the whole chain still loads, as one log, with every original object
        let mut batches = vec![];
        let mut log_end = ORIGIN;
        for name in c.files {
            let (bs, end, format, store_format) = dump_batches(&fixture(name));
            assert_eq!((format, store_format), c.pair, "{name}");
            batches.extend(bs);
            log_end = end;
        }
        let to = tempfile::tempdir().unwrap();
        let dir = to.path().join("p");
        let files: Vec<PathBuf> = c.files.iter().map(|n| fixture(n)).collect();
        let s = rdy_engine::load::load_chain(&files, &dir, None).unwrap();
        assert_eq!(
            (s.dump_format, &s.store_format),
            (c.pair.0, &DumpStore::Legacy(c.pair.1)),
            "{}",
            c.expect
        );
        assert_eq!(s.chain.len(), 3);
        check_loaded(
            c.expect, &dir, &batches, log_end, c.pair.1, c.pair, c.holder,
        );
    }
}
