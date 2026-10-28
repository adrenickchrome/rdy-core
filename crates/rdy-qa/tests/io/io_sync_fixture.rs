//! ha17: the sync batch vectors of store format 15 (D-D2NPN: records as store frames, a header, a
//! trailer, dependency order, verify-on-receive; D-DAGNB: the repo id; D-3M81W, D-CC90E: a
//! removed payload travels as its placeholder and its approved proof). The fixture is one full
//! batch, the history of the every-kind holder's relayed tag `v3.0.0`, exported from the holder
//! that `io_dump_fixture_s15.rs` dumps (`ha17_write_fixtures_once` writes both).
//!
//! These vectors pin the bytes of sync batch format 1 at store format 15. They add no promise
//! that a later rdy reads them: sync batch format 1 has no pre-release compatibility promise, so
//! this rdy refuses them with E_STORE_FORMAT (`sync batch format 1 is not supported`), and the
//! tests pin that refusal. The fixture's bytes stay frozen. The tests read the same tag's batch
//! that this rdy exports from the loaded holder dump, in sync batch format 2: the fixture's
//! present records byte for byte, each removed record's proof with its encoding (0) after its
//! kind, under a header that names the store's feature manifest. The current format's own
//! vectors are the `v2-f1-e0` set (`io_feature_fixture`).
//!
//! The test of the take splits that current batch at record boundaries into separate batches
//! (common root and repo fields, their own prerequisites and trailers), checks each one alone,
//! then takes their union, in reverse order, into a new holder, where every frame lands at an
//! offset of the new log. The source's spans remain historical evidence and are never receiver places; the
//! holder derives its removals again from its own log after its index is deleted.
use rdy_engine::Rdy;
use rdy_engine::load::load;
use rdy_engine::stream_check::check_batch;
use rdy_engine::stream_take::take;
use rdy_model::Hash;
use rdy_model::Tag;
use rdy_qa::every_kind;
use rdy_store::format::{REF_STORE_FEATURES, baseline_features};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use rdy_store::sync_stream::{
    MAGIC, STREAM_FORMAT, StreamHeader, StreamReader, StreamRecord, StreamWriter, rec,
};
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The fixture's blake3, set when it was written.
const SYNC_BLAKE3: &str = "b4e977c6b5fbf6d90b23ab4a9e8727ab10e2852699e8de94e8fb572eeb87a0cf";
/// The blake3 of the expect file's bytes, set when it was written.
const SYNC_EXPECT_BLAKE3: &str = "1b288d272891be45291c7fb77a059cded97ae2c69354b6d2c1c10b6fa68b435d";
pub(crate) const SYNC: &str = "v1-s15.rdysync";
pub(crate) const SYNC_EXPECT: &str = "v1-s15.expect.json";
/// The tag whose history the fixture holds.
pub(crate) const SYNC_TAG: &str = "v3.0.0";
/// The batches the take test splits the fixture into.
const PARTS: usize = 3;
/// The bytes of a trailer frame: length, kind, hash and the 48-byte payload.
const TRAILER_FRAME: u64 = 4 + 1 + 32 + 48;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

pub(crate) fn sync_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/sync")
        .join(name)
}

fn expect() -> Value {
    serde_json::from_str(&std::fs::read_to_string(sync_fixture(SYNC_EXPECT)).unwrap()).unwrap()
}

/// The header and records of `batch`, in wire order, and its trailer's totals.
fn read_batch(batch: &[u8]) -> (StreamHeader, Vec<StreamRecord>, Value) {
    let (mut rd, header) = StreamReader::open(batch).unwrap();
    let mut records = vec![];
    while let Some(r) = rd.next().unwrap() {
        records.push(r);
    }
    let totals = rd.finish().unwrap();
    (header, records, serde_json::to_value(totals).unwrap())
}

/// One record as the expect file states it: its kind, hash and form, the length of its object,
/// and for a placeholder its apply and residue.
fn record_fact(r: &StreamRecord) -> Value {
    match r {
        StreamRecord::Present { key, payload, .. } => json!({
            "kind": key.kind,
            "hash": key.hash.to_string(),
            "form": "present",
            "len": payload.len(),
        }),
        StreamRecord::Dropped {
            key,
            original_len,
            apply,
            residue,
            ..
        } => json!({
            "kind": key.kind,
            "hash": key.hash.to_string(),
            "form": "dropped",
            "len": original_len,
            "apply": apply.to_string(),
            "residue": residue.map(|h| h.to_string()),
        }),
    }
}

/// What the batch `bytes` states: `{"header", "records", "totals"}`.
pub(crate) fn stream_facts(bytes: &[u8]) -> Value {
    let (header, records, totals) = read_batch(bytes);
    json!({
        "header": serde_json::to_value(&header).unwrap(),
        "records": records.iter().map(record_fact).collect::<Vec<_>>(),
        "totals": totals,
    })
}

/// The upload target of the batch `bytes`: its root TAG's name and landing, from the record.
fn target_of(bytes: &[u8]) -> UploadTarget {
    let (header, records, _) = read_batch(bytes);
    let root = records
        .iter()
        .find_map(|r| match r {
            StreamRecord::Present { key, payload, .. } if *key == header.root() => Some(payload),
            _ => None,
        })
        .expect("the batch holds its root");
    let tag: Tag = postcard::from_bytes(root).unwrap();
    UploadTarget {
        tag: tag.name,
        landing: tag.landing,
    }
}

/// Every frame of `r`'s log up to its end: its place, kind and hash, and whether a removal
/// dropped it.
fn frames(r: &Rdy) -> Vec<(Location, u8, Hash, bool)> {
    let mut out = vec![];
    r.store()
        .reader()
        .for_each_record_from(ORIGIN, r.store().log_end(), |loc, f, _| {
            match f {
                StoredFrame::Present { kind, hash, .. } => out.push((loc, kind, hash, false)),
                StoredFrame::Dropped { kind, hash, .. } => out.push((loc, kind, hash, true)),
            }
            Ok(())
        })
        .unwrap();
    out
}

/// What the holder at `r` reads: its facts, identity and projections.
fn holder_reads(r: &Rdy) -> Value {
    let mut f = every_kind::facts(r);
    f["identity"] = every_kind::identity_facts(r, None);
    f["projections"] = every_kind::projections(r);
    f
}

/// A take of the fixture's batch `bytes` split into [`PARTS`] batches: each is checked alone,
/// then all are taken in reverse order into a new holder at `<dir>/holder` with the batch's repo
/// id. Returns the open holder, its root, and `{"batches", "outcome", "holder"}`: each batch's
/// check report (its prerequisites, keys and totals), the take's reply without its log place,
/// and what the holder reads.
pub(crate) fn take_split(bytes: &[u8], dir: &Path) -> (Rdy, PathBuf, Value) {
    let (header, ..) = read_batch(bytes);
    let parts = every_kind::split_batch(bytes, PARTS);
    assert_eq!(parts.len(), PARTS);
    let mut reports = vec![];
    let mut files = vec![];
    std::fs::create_dir_all(dir.join("up")).unwrap();
    for (i, p) in parts.iter().enumerate() {
        let report = check_batch(p.as_slice(), Some(header.repo_id)).unwrap();
        reports.push(json!({
            "prerequisites": serde_json::to_value(&report.header.prerequisites).unwrap(),
            "keys": serde_json::to_value(&report.keys).unwrap(),
            "records": report.records,
            "bytes": report.bytes,
            "digest": report.digest.to_string(),
        }));
        let path = dir.join(format!("up/part-{i}.rdysync"));
        std::fs::write(&path, p).unwrap();
        files.push(path);
    }
    let root = dir.join("holder");
    let mut h = Rdy::create_holder(&root, header.repo_id).unwrap();
    let opened: Vec<File> = files.iter().rev().map(|p| File::open(p).unwrap()).collect();
    let got = take(&mut h, &opened, target_of(bytes)).unwrap();
    let outcome = json!({
        "records_taken": got.records_taken,
        "frames_written": got.frames_written,
        "redacted_objects": got.redacted_objects,
        "tag": got.tag,
        "tag_hash": got.tag_hash.to_string(),
        "landing": got.landing.to_string(),
        "canon": got.canon.map(|h| h.to_string()),
        "unchanged": got.unchanged,
    });
    let reads = holder_reads(&h);
    (
        h,
        root,
        json!({"batches": reports, "outcome": outcome, "holder": reads}),
    )
}

/// The batch of the fixture's tag exported by this rdy, at its store format: the every-kind
/// holder dump of store format 15 loaded (and so upgraded), then the tag's history written by
/// the stream export. Returns the batch and the loaded holder's directory.
fn current_batch(dir: &Path) -> Vec<u8> {
    let to = dir.join("h");
    load(
        &crate::io_dump_fixture_s15::fixture(crate::io_dump_fixture_s15::HOLDER_DUMP),
        &to,
    )
    .unwrap();
    let held = Rdy::open_holder(&to).unwrap();
    let tag = held
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == SYNC_TAG)
        .unwrap()
        .1;
    let bytes = every_kind::export_tag(&held, tag);
    held.close().unwrap();
    bytes
}

/// The frozen fixture is refused by this rdy: sync batch format 1 is not read.
fn assert_old_format_refused(e_code: &str, message: &str) {
    assert_eq!(e_code, "E_STORE_FORMAT", "{message}");
    assert_eq!(message, "sync batch format 1 is not supported");
}

/// The facts the expect file states for the fixture, in this rdy's sync batch format: the header
/// names stream format 2 and the store's features in place of store format 15, and the trailer's
/// byte count and digest (which cover the header record and the longer removal proofs) are
/// `bytes` and `digest`. Every record and the record count are the fixture's.
fn current_facts(expect: &Value, bytes: u64, digest: &str) -> Value {
    let mut header = expect["header"].clone();
    let h = header.as_object_mut().unwrap();
    assert_eq!(h.remove("store_format"), Some(json!(15)));
    h.insert("stream_format".into(), json!(STREAM_FORMAT));
    h.insert("features".into(), json!(baseline_features()));
    let mut totals = expect["totals"].clone();
    totals["bytes"] = json!(bytes);
    totals["digest"] = json!(digest);
    json!({"header": header, "records": expect["records"], "totals": totals})
}

/// The frames of a sync batch after its header, up to its trailer, as (kind, payload), read by
/// their framing alone (a batch of another format included).
fn raw_frames(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut at = 12 + 37 + u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let mut out = vec![];
    while at < bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = bytes[at + 4];
        if kind == rec::TRAILER {
            break;
        }
        out.push((kind, bytes[at + 37..at + 37 + len].to_vec()));
        at += 37 + len;
    }
    out
}

#[test]
fn ha17_stream_fixture_has_exact_wire_bytes() {
    let bytes = std::fs::read(sync_fixture(SYNC)).unwrap();
    assert_eq!(Hash::of(&bytes).to_string(), SYNC_BLAKE3);
    let text = std::fs::read(sync_fixture(SYNC_EXPECT)).unwrap();
    assert_eq!(Hash::of(&text).to_string(), SYNC_EXPECT_BLAKE3);
    let expect = expect();
    // the prelude: the magic and the stream format, little-endian
    assert_eq!(&bytes[..8], MAGIC);
    assert_eq!(&bytes[..8], b"RDYSYNC\n");
    assert_eq!(STREAM_FORMAT, 2);
    assert_eq!(
        bytes[8..12],
        1u32.to_le_bytes(),
        "the frozen batch is of format 1"
    );
    // the header frame: its length, kind 0xF0 and the hash of its payload; the payload is the
    // store format, the repo id, purpose 0, the root TAG's hash and no prerequisite
    let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    assert_eq!(bytes[16], 0xF0);
    assert_eq!(rec::HEADER, 0xF0);
    let payload = &bytes[49..49 + len];
    assert_eq!(bytes[17..49], Hash::of(payload).0);
    assert_eq!(len, 4 + 16 + 1 + 32 + 4);
    assert_eq!(payload[..4], 15u32.to_le_bytes());
    let header = &expect["header"];
    assert_eq!(header["store_format"], 15);
    assert_eq!(hex(&payload[4..20]), header["repo_id"]);
    assert_eq!(payload[20], 0, "purpose tag_history");
    assert_eq!(hex(&payload[21..53]), header["tag"]);
    assert_eq!(payload[53..57], 0u32.to_le_bytes());
    // the trailer frame ends the file: 48 bytes of payload after the bytes the totals count
    assert_eq!(
        expect["totals"]["bytes"].as_u64().unwrap() + TRAILER_FRAME,
        bytes.len() as u64,
        "the trailer frame ends the file"
    );
    let trailer_at = bytes.len() - TRAILER_FRAME as usize;
    assert_eq!(bytes[trailer_at..trailer_at + 4], 48u32.to_le_bytes());
    assert_eq!(bytes[trailer_at + 4], 0xFF);
    assert_eq!(rec::TRAILER, 0xFF);
    for canary in [
        every_kind::HOLDER_CANARY.as_bytes(),
        every_kind::HOLDER_BLOB,
    ] {
        assert!(
            !bytes.windows(canary.len()).any(|w| w == canary),
            "a removed payload is in the batch"
        );
    }
    // this rdy reads no batch of another store format: the reader and the batch check refuse
    // the frozen bytes by their header
    let e = StreamReader::open(bytes.as_slice()).err().expect("refused");
    assert_old_format_refused(e.code, &e.message);
    let e = check_batch(bytes.as_slice(), None).unwrap_err();
    assert_old_format_refused(e.code, &e.message);

    // the export of the same tag from the loaded holder dump, in sync batch format 2: the
    // fixture's present records byte for byte, each removal proof with its encoding after its
    // kind, and a header that names the store's features
    let to = tempfile::tempdir().unwrap();
    let now = current_batch(to.path());
    assert_eq!(now[..8], bytes[..8], "the magic");
    assert_eq!(now[8..12], 2u32.to_le_bytes());
    let json = baseline_features().to_json();
    let now_len = u32::from_le_bytes(now[12..16].try_into().unwrap()) as usize;
    assert_eq!(now_len, len + json.len(), "the header names the manifest");
    let now_header = &now[49..49 + now_len];
    assert_eq!(now_header[..4], (json.len() as u32).to_le_bytes());
    assert_eq!(now_header[4..4 + json.len()], json[..]);
    assert_eq!(
        now_header[4 + json.len()..],
        payload[4..],
        "the repo, purpose, root and prerequisites"
    );
    assert_eq!(now[17..49], Hash::of(now_header).0);
    let (then_frames, now_frames) = (raw_frames(&bytes), raw_frames(&now));
    assert_eq!(then_frames.len(), now_frames.len());
    for ((tk, tp), (nk, np)) in then_frames.iter().zip(&now_frames) {
        assert_eq!(tk, nk, "every record keeps its kind");
        if *tk == rec::DROPPED {
            assert_eq!(np.len(), tp.len() + 2);
            assert_eq!(np[0], tp[0], "the logical kind");
            assert_eq!(np[1..3], [0, 0], "encoding 0");
            assert_eq!(np[3..], tp[1..], "the proof, byte for byte");
        } else {
            assert_eq!(np, tp, "a present record, byte for byte");
        }
    }
    // read back: the expect file's header, records and totals in this sync batch format
    let (rh, records, totals) = read_batch(&now);
    let digest = totals["digest"].as_str().unwrap().to_string();
    assert_ne!(digest, expect["totals"]["digest"], "the header is covered");
    assert_eq!(
        stream_facts(&now),
        current_facts(&expect, totals["bytes"].as_u64().unwrap(), &digest),
        "the header, every record and the totals"
    );
    // the placeholders keep their removed objects' identities: the prose's versions, the changes
    // that embed them, and the blob's chunk, which has no residue
    let dropped: BTreeSet<u8> = records
        .iter()
        .filter(|r| matches!(r, StreamRecord::Dropped { .. }))
        .map(|r| r.key().kind)
        .collect();
    assert_eq!(
        dropped,
        BTreeSet::from([kinds::BLOCK, kinds::CHANGE, kinds::BLOB_CHUNK])
    );
    for r in &records {
        if let StreamRecord::Dropped { key, residue, .. } = r {
            assert_eq!(residue.is_none(), key.kind == kinds::BLOB_CHUNK, "{key:?}");
        }
    }
    // the same header and records encode to the same bytes
    let mut w = StreamWriter::new(Vec::new(), &rh).unwrap();
    for r in &records {
        w.record(r).unwrap();
    }
    let (again, _) = w.finish().unwrap();
    assert_eq!(again, now, "the encoding is the export's");
    // the batch check reads the keys in wire order and the trailer's digest
    let report = check_batch(now.as_slice(), Some(rh.repo_id)).unwrap();
    let keys: Vec<ObjectKey> = records.iter().map(StreamRecord::key).collect();
    assert_eq!(report.keys, keys);
    assert_eq!(report.digest.to_string(), digest);
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `v` with each batch report's digest and byte count removed: they cover each batch's header
/// record, which names the store's format, and its removal proofs, whose length the sync batch
/// format sets.
fn without_batch_digests(mut v: Value) -> Value {
    for b in v["batches"].as_array_mut().unwrap() {
        let b = b.as_object_mut().unwrap();
        b.remove("digest");
        b.remove("bytes");
    }
    v
}

/// The ref updates of `r`'s log that set `name`, as identity keys (`8:<hash>`).
fn ref_update_keys(r: &Rdy, name: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    r.store()
        .reader()
        .for_each_record_from(ORIGIN, r.store().log_end(), |_, f, _| {
            if let StoredFrame::Present {
                kind: kinds::REF_UPDATE,
                hash,
                payload,
            } = f
            {
                let u: kinds::RefUpdate = postcard::from_bytes(payload).unwrap();
                if u.name == name {
                    out.insert(format!("{}:{hash}", kinds::REF_UPDATE));
                }
            }
            Ok(())
        })
        .unwrap();
    out
}

/// What the take of the current batch into holder `h` reports and reads (`got`), in the expect
/// file's terms at store format 15: each batch report's digest and byte count are removed (they
/// cover the batch's header and its removal proofs), and the holder's own format ref, which a
/// new holder writes as `meta/store_features`, and the one ref update that writes it, are the
/// expect file's `meta/store_format` and its update. Everything else is compared as it is.
fn at_current_format(h: &Rdy, mut got: Value) -> Value {
    let want = expect()["taken"].clone();
    let refs = got["holder"]["refs"].as_object_mut().unwrap();
    assert_eq!(
        refs.remove(REF_STORE_FEATURES),
        Some(json!(hex(&baseline_features().to_json())))
    );
    refs.insert(
        "meta/store_format".into(),
        want["holder"]["refs"]["meta/store_format"].clone(),
    );
    let keys = |v: &Value| -> BTreeSet<String> {
        v["holder"]["identity"]["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap().to_string())
            .collect()
    };
    let (now, then) = (keys(&got), keys(&want));
    let new: Vec<&String> = now.difference(&then).collect();
    let gone: Vec<&String> = then.difference(&now).collect();
    // the holder's features ref update, and each later ref update, whose salt (the log end it
    // was written at) moved with the longer first batch
    assert_eq!(new.len(), gone.len(), "{new:?} {gone:?}");
    let prefix = format!("{}:", kinds::REF_UPDATE);
    assert!(
        new.iter().chain(&gone).all(|k| k.starts_with(&prefix)),
        "only ref updates differ: {new:?} {gone:?}"
    );
    assert!(
        new.iter()
            .any(|k| ref_update_keys(h, REF_STORE_FEATURES).contains(*k)),
        "the holder's features ref update is among them: {new:?}"
    );
    got["holder"]["identity"]["keys"] = want["holder"]["identity"]["keys"].clone();
    without_batch_digests(got)
}

#[test]
fn ha17_split_batches_take_at_different_offsets() {
    let expect = expect();
    // the frozen batch is no input of this rdy: its check and its take refuse it by its store
    // format, and the take writes nothing
    let frozen = std::fs::read(sync_fixture(SYNC)).unwrap();
    let e = check_batch(frozen.as_slice(), None).unwrap_err();
    assert_old_format_refused(e.code, &e.message);
    let to = tempfile::tempdir().unwrap();
    let repo: rdy_model::RepoId = expect["header"]["repo_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let mut h = Rdy::create_holder(&to.path().join("refused"), repo).unwrap();
    let end = h.store().log_end();
    let frozen_file = to.path().join("frozen.rdysync");
    std::fs::write(&frozen_file, &frozen).unwrap();
    let target = UploadTarget {
        tag: SYNC_TAG.into(),
        landing: expect["taken"]["outcome"]["landing"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
    };
    let e = take(&mut h, &[File::open(&frozen_file).unwrap()], target).unwrap_err();
    assert_old_format_refused(e.code, &e.message);
    assert_eq!(h.store().log_end(), end, "the refused take wrote nothing");
    h.close().unwrap();

    // the same batch at the current store format: this rdy's own export of the tag
    let bytes = current_batch(&to.path().join("source"));
    let (header, records, _) = read_batch(&bytes);
    assert_eq!(header.features, baseline_features());
    let parts = every_kind::split_batch(&bytes, PARTS);
    // separate valid batches: the common root and repo fields, their own prerequisites and their
    // own trailers; only the last one holds the root
    let mut digests = BTreeSet::new();
    let mut held: Vec<ObjectKey> = vec![];
    for (i, p) in parts.iter().enumerate() {
        let (h, rs, totals) = read_batch(p);
        assert_eq!(
            (h.stream_format, &h.features, h.repo_id, h.purpose, h.tag),
            (
                header.stream_format,
                &header.features,
                header.repo_id,
                header.purpose,
                header.tag
            )
        );
        assert_eq!(rs.iter().any(|r| r.key() == header.root()), i == PARTS - 1);
        if i < PARTS - 1 {
            assert!(h.prerequisites.contains(&header.root()), "batch {i}");
        }
        // a later batch lists what it needs of the batches before it
        if i > 0 {
            assert!(!h.prerequisites.is_empty(), "batch {i}");
        }
        assert!(digests.insert(totals["digest"].as_str().unwrap().to_string()));
        held.extend(rs.iter().map(StreamRecord::key));
    }
    assert_eq!(
        held,
        records.iter().map(StreamRecord::key).collect::<Vec<_>>(),
        "the batches hold the fixture's records, split at record boundaries"
    );
    let (h, root, got) = take_split(&bytes, &to.path().join("take"));
    assert_eq!(
        at_current_format(&h, got.clone()),
        without_batch_digests(expect["taken"].clone()),
        "the take and what the holder reads"
    );
    assert_eq!(got["outcome"]["unchanged"], false);
    assert_eq!(got["holder"]["identity"]["mode"], "holder");
    assert_eq!(got["holder"]["identity"]["canon"], Value::Null);
    // every placeholder is a dropped frame of the new log, at a place of that log; no place is
    // the source's: the request's spans name the holder's places, which were other ones
    let rm = every_kind::removal(&h);
    let here: BTreeSet<(Location, u8, Hash)> = frames(&h)
        .into_iter()
        .filter(|f| f.3)
        .map(|(l, k, hash, _)| (l, k, hash))
        .collect();
    let placeholders: BTreeSet<(u8, Hash)> = records
        .iter()
        .filter(|r| matches!(r, StreamRecord::Dropped { .. }))
        .map(|r| (r.key().kind, r.key().hash))
        .collect();
    assert_eq!(
        here.iter()
            .map(|(_, k, h)| (*k, *h))
            .collect::<BTreeSet<_>>(),
        placeholders
    );
    let source: BTreeSet<(Location, u8, Hash)> = rm
        .record
        .manifest
        .spans
        .iter()
        .map(|s| {
            (
                Location {
                    segment: s.place.segment,
                    offset: s.place.offset,
                },
                s.kind,
                s.hash,
            )
        })
        .collect();
    assert!(
        here.is_disjoint(&source),
        "a source span is never a receiver place: {here:?} {source:?}"
    );
    for (_, kind, hash) in &here {
        assert_eq!(
            h.store().redaction_of(*kind, hash).unwrap(),
            Some(rm.request),
            "{kind} {hash}"
        );
        assert!(h.store().get_kind(*kind, hash).is_err(), "{kind} {hash}");
    }
    let end = h.store().log_end();
    let reads = holder_reads(&h);
    h.close().unwrap();
    // recovery with no map: the derived index is deleted, and the open derives every removal
    // again from the sealed log alone
    std::fs::remove_file(root.join(".rdy/store/index.redb")).unwrap();
    let mut h = Rdy::open_holder(&root).unwrap();
    assert_eq!(holder_reads(&h), reads, "after the index is rebuilt");
    assert!(frames(&h).iter().all(|f| f.1 != 12), "no record of kind 12");
    h.reindex().unwrap();
    assert_eq!(holder_reads(&h), reads, "after a reindex");
    h.fsck().unwrap();
    // the same union again appends nothing
    let files: Vec<File> = (0..PARTS)
        .map(|i| File::open(to.path().join(format!("take/up/part-{i}.rdysync"))).unwrap())
        .collect();
    let again = take(&mut h, &files, target_of(&bytes)).unwrap();
    assert!(again.unchanged, "{again:?}");
    assert_eq!(h.store().log_end(), end);
    h.close().unwrap();
}
