//! rdx15: the dump promise for store format 13 (D-69AY3: each released dump pair is a permanent
//! promise; D-3M81W: rdy "drops their bytes and keeps their hashes"; D-600CW: "Content may leave
//! the store; the record that it left never does."). The quality gate keeps one fixture for each
//! pair of dump format and store format: `(2, 13)` is released at the merge of Redaction, with
//! this file. Every later rdy loads it with every hash kept. Write it once: never rewrite it, and
//! never regenerate it to fix a later reader.
//!
//! The fixture is a dump of the every-kind project, which is a clone (O41) of a source whose last
//! landing is a person's approval of a content removal: the dump holds real `DROPPED` records
//! for a block version, the changes that embed it (a duplicate frame among them) and a blob's
//! chunks (one shared with another blob), every kind of store format 13, the repo id and the
//! origin. As the earlier fixtures, it was written on a store whose segments hold
//! `FIXTURE_SEGMENT_MAX` bytes, so the load must put each batch at its source place.
//!
//! A later store format may add kinds and refs, never re-encode (D-F0904). A load of this fixture
//! then appends one batch after the fixture's log: it sets `meta/store_format` to that rdy's
//! store format (FW-UP1). The load test allows exactly that batch.
use rdy_engine::{Rdy, load::load};
use rdy_model::redaction::{CONTENT_REMOVED, ImpactKind, ImpactRelation, RedactionResidue};
use rdy_model::{BlockId, Hash};
use rdy_qa::every_kind;
use rdy_store::dump::{DUMP_FORMAT, DumpBatch, DumpItem, DumpReader, Frame};
use rdy_store::format::{REF_STORE_FORMAT, STORE_FORMAT, encode_u32};
use rdy_store::kinds::{self, BlobManifest, RefUpdate};
use rdy_store::meta::{REF_ORIGIN, REF_REPO_ID};
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The fixture's blake3, set when it was written. A change here needs a founder decision.
const FIXTURE_BLAKE3: &str = "bf177175d1f8fc964287579a00739a20fd90bc4d78184270436cc88409292be2";
/// The blake3 of the expect file's bytes, set when it was written. A change here needs a founder
/// decision.
const EXPECT_BLAKE3: &str = "05e4c3be472db581143745df737b8022dd4e010783641b9dc5c194869d4e5f0a";
/// The ref families of the v2-s13 fixture (store format 13): the sixteen of the v1-s12 fixture.
/// Store format 13 adds kinds, no ref family: a redaction has no ref. Frozen with this fixture: a
/// later store format that adds a family adds it to `every_kind::FAMILIES`, never here.
const V13_FAMILIES: [&str; 16] = [
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
const FIXTURE_ID_SEED: u64 = 13;
const DUMP: &str = "v2-s13.rdydump";
const EXPECT: &str = "v2-s13.expect.json";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name)
}

fn expect() -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(EXPECT)).unwrap()).unwrap()
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
    (out, h.log_end, format, h.store_format)
}

/// Each batch's start as `[segment, offset]`, in log order.
fn starts(bs: impl IntoIterator<Item = Location>) -> Value {
    json!(
        bs.into_iter()
            .map(|l| [u64::from(l.segment), l.offset])
            .collect::<Vec<_>>()
    )
}

/// One record of the loaded log: its kind, its hash, and for a dropped frame its apply and
/// residue.
type Walked = (u8, Hash, Option<(Hash, Option<Hash>)>);

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

/// What the removal left, as the store reads it: the request, its apply and report hash, the
/// manifest's selected versions, blobs and shared blobs, every dropped frame of the log with its
/// place, original kind, hash and length and the apply and residue that authorize its absence,
/// the stub each removed version reads as, the manifest's inside rows (the tags, landings, state
/// snapshots and quoting records it names, by their unchanged hashes), and every tag's landing.
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
                "order": b.order,
                "body": b.body,
                "fields": b.fields.len(),
                "links": b.links.len(),
                "version": ident.version.to_string(),
                "content_hash": ident.content_hash.to_string(),
                "frozen": ident.frozen,
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

/// Writes the fixture once. Run by hand, never by the gate:
/// `cargo test -p rdy-qa --release --test io rdx15_write_the_v2_s13_fixture -- --ignored`.
#[test]
#[ignore]
fn rdx15_write_the_v2_s13_fixture() {
    let (dump, expect) = (fixture(DUMP), fixture(EXPECT));
    assert!(
        !dump.exists() && !expect.exists(),
        "the v2-s13 fixture is written once: never rewrite it"
    );
    assert_eq!(
        (DUMP_FORMAT, STORE_FORMAT),
        (2, 13),
        "the v2-s13 fixture holds dump format 2 and store format 13"
    );
    std::fs::create_dir_all(dump.parent().unwrap()).unwrap();
    rdy_model::seed_ids_for_tests(FIXTURE_ID_SEED);
    let src = every_kind::rolling_project(FIXTURE_SEGMENT_MAX);
    rdy_model::unseed_ids_for_tests();
    let r = Rdy::open(src.path()).unwrap();
    // the dump goes to a scratch file first, so a fixture member is only ever created whole
    let scratch = tempfile::tempdir().unwrap();
    let tmp = scratch.path().join(DUMP);
    let d = r.read().dump(&tmp).unwrap();
    assert_eq!((d.dump_format, d.store_format), (2, 13));
    let mut facts = every_kind::facts(&r);
    facts["redaction"] = removal_facts(&r);
    r.close().unwrap();
    facts["segment_max"] = json!(FIXTURE_SEGMENT_MAX);
    facts["batch_starts"] = starts(dump_batches(&tmp).0.iter().map(|b| b.start));
    let text = serde_json::to_string_pretty(&facts).unwrap() + "\n";
    let bytes = std::fs::read(&tmp).unwrap();
    write_new(&dump, &bytes);
    write_new(&expect, text.as_bytes());
    eprintln!("blake3 {}", Hash::of(&bytes));
    eprintln!("expect blake3 {}", Hash::of(text.as_bytes()));
}

#[test]
fn rdx15_the_v2_s13_fixture_is_never_rewritten() {
    // A change to either file needs a founder decision. The checksums are fixed here, apart from
    // any writer: a later writer that writes other bytes changes nothing in this check.
    let bytes = std::fs::read(fixture(DUMP)).unwrap();
    assert_eq!(Hash::of(&bytes).to_string(), FIXTURE_BLAKE3);
    let bytes = std::fs::read(fixture(EXPECT)).unwrap();
    assert_eq!(Hash::of(&bytes).to_string(), EXPECT_BLAKE3);
}

#[test]
fn rdx15_the_v2_s13_fixture_loads_with_every_hash() {
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    let s = load(&fixture(DUMP), &dir).unwrap();
    assert_eq!((s.dump_format, s.store_format), (2, 13));
    let expect = expect();
    let (batches, log_end, format, fixture_format) = dump_batches(&fixture(DUMP));
    assert_eq!((format, fixture_format), (2, 13));
    // no selected canary is in the fixture's bytes: each one left with its payload
    let bytes = std::fs::read(fixture(DUMP)).unwrap();
    for canary in [every_kind::CANARY_BODY, every_kind::CANARY_FIELD] {
        assert!(
            !bytes.windows(canary.len()).any(|w| w == canary.as_bytes()),
            "a selected canary is in the fixture"
        );
    }
    let r = Rdy::open(&dir).unwrap();
    let got = every_kind::facts(&r);
    assert_eq!(
        got["landings"], expect["landings"],
        "every landing, in order"
    );
    // every ref holds its value, and no other ref exists; `meta/store_format` names this rdy's
    // store format. The fixture holds its repo id and its origin: a load keeps both.
    let mut want = expect["refs"].clone();
    want[REF_STORE_FORMAT] = json!(hex(&encode_u32(STORE_FORMAT)));
    assert!(want[REF_REPO_ID].is_string(), "the fixture holds a repo id");
    assert!(want[REF_ORIGIN].is_string(), "the fixture holds an origin");
    assert_eq!(got["refs"], want, "every ref");
    // the fixture's own log range: every original kind counted, dropped frames too, and each
    // batch at its source place
    let reader = r.store().reader();
    let mut objects: BTreeMap<String, u64> = BTreeMap::new();
    let mut walked: BTreeMap<Location, Walked> = BTreeMap::new();
    reader
        .for_each_record_from(ORIGIN, log_end, |loc, f, _| {
            let (kind, hash, proof) = match f {
                StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } => {
                    assert_eq!(
                        Hash::of(payload),
                        hash,
                        "a surviving payload keeps its hash"
                    );
                    (kind, hash, None)
                }
                StoredFrame::Dropped {
                    kind,
                    hash,
                    apply,
                    residue,
                    ..
                } => (kind, hash, Some((apply, residue))),
            };
            if kind != kinds::BATCH_COMMIT {
                *objects.entry(kind.to_string()).or_default() += 1;
            }
            walked.insert(loc, (kind, hash, proof));
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
    // past the fixture's log end: nothing while the store format is 13; for a newer store format
    // only the upgrade batch, which holds `meta/store_format` alone
    let mut tail = vec![];
    reader
        .for_each_record_from(log_end, r.store().log_end(), |_, f, _| {
            match f {
                StoredFrame::Present { kind, payload, .. } => tail.push((kind, payload.to_vec())),
                StoredFrame::Dropped { kind, .. } => tail.push((kind, vec![])),
            }
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
    // every object of the fixture is in the loaded store under its own hash and kind; each
    // dropped record is a dropped frame at its place, with the same proof
    let rm = every_kind::removal(&r);
    let mut dropped = 0;
    for b in &batches {
        for f in &b.frames {
            assert!(
                r.store().has_kind(f.kind(), f.hash()).unwrap(),
                "kind {} {} is gone",
                f.kind(),
                f.hash()
            );
            if let Frame::Dropped {
                kind,
                hash,
                apply,
                residue,
                ..
            } = f
            {
                dropped += 1;
                assert!(
                    walked
                        .values()
                        .any(|w| *w == (*kind, *hash, Some((*apply, *residue)))),
                    "dropped {kind} {hash} keeps its proof"
                );
                assert_eq!(*apply, rm.apply, "the one apply authorizes it");
                assert_eq!(
                    r.store().redaction_of(*kind, hash).unwrap(),
                    Some(rm.request),
                    "{kind} {hash} is masked by the request"
                );
            }
        }
    }
    assert!(dropped > 0, "the fixture holds dropped records");
    // each dropped frame is a span of the request's manifest, with its residue; a residue
    // decodes and names the frame's own hash
    for (loc, (kind, hash, proof)) in &walked {
        let Some((apply, residue)) = proof else {
            continue;
        };
        assert_eq!(*apply, rm.apply);
        let span = rm
            .record
            .manifest
            .spans
            .iter()
            .find(|s| s.place.segment == loc.segment && s.place.offset == loc.offset)
            .unwrap_or_else(|| panic!("{loc:?} is a span of the request"));
        assert_eq!(
            (span.kind, span.hash, span.residue),
            (*kind, *hash, *residue)
        );
        if let Some(res) = residue {
            let rec: RedactionResidue = reader.get_typed(kinds::REDACTION_RESIDUE, res).unwrap();
            match rec {
                RedactionResidue::Block(b) => assert_eq!(b.version, *hash),
                RedactionResidue::Change(c) => assert_eq!(c.change, *hash),
            }
        }
    }
    // the request, its apply, its stubs and the tags and landings it names are as written
    assert_eq!(
        removal_facts(&r),
        expect["redaction"],
        "the removal's record"
    );
    for (key, stub) in expect["redaction"]["stubs"].as_object().unwrap() {
        assert_eq!(stub["body"], CONTENT_REMOVED, "{key}");
        assert_eq!(
            (stub["fields"].as_u64(), stub["links"].as_u64()),
            (Some(0), Some(0))
        );
        assert_eq!(stub["version"], key.split_once('@').unwrap().1, "{key}");
    }
    drop(reader);
    // fsck counts the whole log: the fixture's frames, plus each intact data frame of the
    // upgrade batch checked above (none while the store format is 13), which is hashed too
    let upgrade = tail
        .iter()
        .filter(|(kind, _)| *kind != kinds::BATCH_COMMIT)
        .count() as u64;
    let mut want_fsck = expect["fsck"].clone();
    for member in ["frames", "checked_payloads"] {
        want_fsck[member] = json!(want_fsck[member].as_u64().unwrap() + upgrade);
    }
    assert_eq!(got["fsck"], want_fsck, "fsck counts");
    rdy_qa::invariants::check_all(&r).unwrap_or_else(|v| panic!("{v:#?}"));
    r.close().unwrap();
    // the reindex check opens the project itself, so the store lock is released first
    rdy_qa::reindex_check::reindex_reproduces(&dir).unwrap_or_else(|v| panic!("{v:#?}"));
}

#[test]
fn rdx15_the_fixture_covers_all_kinds_and_copy_shapes() {
    let expect = expect();
    for k in every_kind::V13_KINDS {
        assert!(
            expect["objects"][k.to_string()].as_u64().unwrap_or(0) >= 1,
            "kind {k}"
        );
    }
    let refs = expect["refs"].as_object().unwrap();
    for f in V13_FAMILIES {
        assert!(refs.keys().any(|n| n.starts_with(f)), "family {f}");
    }
    // the ref families are unchanged: no ref is a redaction's
    assert!(
        !refs.keys().any(|n| n.contains("redact")),
        "a redaction has no ref: {:?}",
        refs.keys()
    );
    // the copy shapes, read from the loaded fixture
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    load(&fixture(DUMP), &dir).unwrap();
    let r = Rdy::open(&dir).unwrap();
    let rm = every_kind::removal(&r);
    let m = &rm.record.manifest;
    let reader = r.store().reader();
    let row = |kind: ImpactKind, relation: ImpactRelation| {
        m.inside
            .iter()
            .any(|i| i.kind == kind && i.relation == relation)
    };
    // a state snapshot and a tag that reference the removed version
    assert!(row(ImpactKind::StateSnapshot, ImpactRelation::References));
    assert!(row(ImpactKind::Tag, ImpactRelation::References));
    assert!(row(ImpactKind::Landing, ImpactRelation::References));
    // F3(a): a review finding, a tension's advice and an answer's note and relayed words quote
    // the removed version whole; each record is kept, unerasable, under its original hash
    for kind in [ImpactKind::Review, ImpactKind::Tension, ImpactKind::Answer] {
        let quoting: Vec<_> = m
            .inside
            .iter()
            .filter(|i| i.kind == kind && i.relation == ImpactRelation::Quotes)
            .collect();
        assert!(!quoting.is_empty(), "{kind:?} quotes the removed version");
        for q in quoting {
            assert!(!q.erasable, "{kind:?} {} is kept", q.record);
            let h: Hash = q.record.parse().unwrap();
            let k = match kind {
                ImpactKind::Review => kinds::REVIEW,
                ImpactKind::Tension => kinds::TENSION,
                _ => kinds::ANSWER,
            };
            let payload = reader.get_kind(k, &h).unwrap();
            assert_eq!(Hash::of(&payload), h, "{kind:?} keeps its original hash");
        }
    }
    // a blob whose chunk another blob shares: the shared chunk is dropped once for both
    assert_eq!(m.blobs.len(), 1, "one selected blob");
    assert_eq!(m.shared_blobs.len(), 1, "one blob shares a chunk");
    let selected: BlobManifest = reader.get_typed(kinds::BLOB_MANIFEST, &m.blobs[0]).unwrap();
    let shared: BlobManifest = reader
        .get_typed(kinds::BLOB_MANIFEST, &m.shared_blobs[0])
        .unwrap();
    let common: Vec<&Hash> = selected
        .chunks
        .iter()
        .filter(|c| shared.chunks.contains(c))
        .collect();
    assert_eq!(common.len(), 1, "the two blobs share one chunk");
    assert!(
        m.spans
            .iter()
            .any(|s| s.kind == kinds::BLOB_CHUNK && s.hash == *common[0]),
        "the shared chunk is dropped"
    );
    assert!(
        reader.get_blob(&m.shared_blobs[0]).is_err(),
        "it cannot restore the chunk"
    );
    // a duplicate frame: one version logged twice, both copies dropped
    let mut seen: BTreeMap<(u8, Hash), usize> = BTreeMap::new();
    for s in &m.spans {
        *seen.entry((s.kind, s.hash)).or_default() += 1;
    }
    assert!(
        seen.iter().any(|((k, _), n)| *k == kinds::BLOCK && *n >= 2),
        "a duplicate block frame: {seen:?}"
    );
    // the embedded block: the change that created the removed version is dropped too, and its
    // residue keeps the sibling op, whose block still reads in full
    let versions: BTreeSet<Hash> = m.versions.iter().map(|(_, v)| *v).collect();
    let mut sibling = None;
    for s in m.spans.iter().filter(|s| s.kind == kinds::CHANGE) {
        let res: RedactionResidue = reader
            .get_typed(kinds::REDACTION_RESIDUE, &s.residue.unwrap())
            .unwrap();
        let RedactionResidue::Change(c) = res else {
            panic!("a change span has a change residue")
        };
        let produced: Vec<Hash> = c
            .ops
            .iter()
            .filter_map(|op| match op {
                rdy_model::redaction::ResidualOp::Create { version, .. }
                | rdy_model::redaction::ResidualOp::Update { version, .. } => Some(*version),
                _ => None,
            })
            .collect();
        assert!(
            produced.iter().any(|v| versions.contains(v)),
            "it embeds a removed version"
        );
        if let Some(v) = produced.iter().find(|v| !versions.contains(v)) {
            sibling = Some(*v);
        }
    }
    let sibling = sibling.expect("a multi-op change with an unaffected sibling op");
    let b = reader.get_block(&sibling).unwrap();
    assert!(!b.is_redacted(), "the sibling op's block reads");
    assert_eq!(b.body, every_kind::SIBLING_BODY);
    // every removable kind has a dropped record in the dump
    let (batches, ..) = dump_batches(&fixture(DUMP));
    let dropped: BTreeSet<u8> = batches
        .iter()
        .flat_map(|b| &b.frames)
        .filter(|f| f.present_payload().is_none())
        .map(Frame::kind)
        .collect();
    assert_eq!(
        dropped,
        BTreeSet::from([kinds::BLOCK, kinds::CHANGE, kinds::BLOB_CHUNK])
    );
    drop(reader);
    r.close().unwrap();
}
