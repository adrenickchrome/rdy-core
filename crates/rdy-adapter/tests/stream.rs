//! ha14 (D-D2NPN, D-DAGNB, D-WDKWQ): the stateless batch check and the library stream export
//! through the adapter. Core owns the bytes, the errors and the limits; the adapter delegates, so
//! every result here is compared with the core call it stands for. The project is built with core
//! calls in a closed store first, then read through the adapter.

use rdy_adapter::stream::{
    BatchReport, ObjectKey, ObjectPresence, StreamError, StreamHeader, StreamPurpose, StreamReader,
    StreamRecord, StreamTotals, StreamWriter, check_batch,
};
use rdy_adapter::{Hash, RdyError, RepoId, Snapshot, StoreHandle};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest, stream_check, stream_export};
use rdy_model::Role;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "ha14".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, title: &str) {
    let p = r
        .propose(&session(Role::Writer), "work", title, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

struct Fixture {
    dir: tempfile::TempDir,
    repo: RepoId,
    tag: Hash,
    /// What core itself exports for `tag`, with nothing known.
    bytes: Vec<u8>,
    report: BatchReport,
    /// Core's export with the first two keys of `report` known.
    known: BTreeSet<ObjectKey>,
    partial: Vec<u8>,
    partial_report: BatchReport,
}

/// A closed project with two landings, tagged v1.0.0 after the second, and core's own export of
/// that tag taken before any adapter handle opens the store.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "First."}
        ]),
    );
    land(&mut r, "first");
    write(
        &mut r,
        json!([{"op": "create", "type": "prose", "id": "$q", "parent": w.created["$d"], "body": "Second."}]),
    );
    land(&mut r, "second");
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let tag = r.store().tags().unwrap()[0].1;
    let repo = r.store().repo_id().unwrap().unwrap();
    let mut bytes = Vec::new();
    let report = stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut bytes).unwrap();
    let known: BTreeSet<ObjectKey> = report.keys.iter().take(2).copied().collect();
    let mut partial = Vec::new();
    let partial_report = stream_export::write(&r.read(), tag, &known, &mut partial).unwrap();
    r.close().unwrap();
    Fixture {
        dir,
        repo,
        tag,
        bytes,
        report,
        known,
        partial,
        partial_report,
    }
}

fn export(snap: &Snapshot, tag: &str, known: &BTreeSet<ObjectKey>) -> (Vec<u8>, BatchReport) {
    let mut out = Vec::new();
    let report = snap.write_stream(tag, known, &mut out).unwrap();
    (out, report)
}

fn same_error(a: &RdyError, b: &RdyError) {
    assert_eq!((&a.code, &a.message, &a.fix), (&b.code, &b.message, &b.fix));
}

#[test]
fn ha14_adapter_batch_matches_core_bytes_and_reports() {
    let f = fixture();
    let handle = StoreHandle::open(f.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();

    // the export is core's bytes and core's report, by stored name or by version
    let (bytes, report) = export(&snap, "v1.0.0", &BTreeSet::new());
    assert_eq!(bytes, f.bytes);
    assert_eq!(report, f.report);
    assert_eq!(export(&snap, "1.0.0", &BTreeSet::new()).0, f.bytes);
    assert_eq!(report.header.tag, f.tag);
    assert_eq!(report.header.repo_id, f.repo);
    assert_eq!(report.header.purpose, StreamPurpose::TagHistory);
    assert_eq!(report.bytes + 85, bytes.len() as u64);

    // the check is core's check: same report, with or without the repo to match
    assert_eq!(check_batch(bytes.as_slice(), None).unwrap(), report);
    assert_eq!(
        check_batch(bytes.as_slice(), Some(f.repo)).unwrap(),
        stream_check::check_batch(bytes.as_slice(), Some(f.repo)).unwrap()
    );

    // known keys leave their objects out and become the header's prerequisites, as in core
    let (partial, partial_report) = export(&snap, "v1.0.0", &f.known);
    assert_eq!(partial, f.partial);
    assert_eq!(partial_report, f.partial_report);
    assert_eq!(partial_report.keys.len() + 2, report.keys.len());
    assert!(!partial_report.header.prerequisites.is_empty());

    // errors, messages and fixes are core's
    let wrong = RepoId([9; 16]);
    same_error(
        &check_batch(bytes.as_slice(), Some(wrong)).unwrap_err(),
        &stream_check::check_batch(bytes.as_slice(), Some(wrong)).unwrap_err(),
    );
    assert_eq!(
        check_batch(bytes.as_slice(), Some(wrong)).unwrap_err().code,
        "E_REPO_MISMATCH"
    );
    let cut = &bytes[..bytes.len() - 1];
    same_error(
        &check_batch(cut, None).unwrap_err(),
        &stream_check::check_batch(cut, None).unwrap_err(),
    );
    assert_eq!(
        check_batch(cut, None).unwrap_err().code,
        "E_STREAM_TRUNCATED"
    );
    let mut flipped = bytes.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0xff;
    same_error(
        &check_batch(flipped.as_slice(), None).unwrap_err(),
        &stream_check::check_batch(flipped.as_slice(), None).unwrap_err(),
    );
    let mut out = Vec::new();
    let e = snap
        .write_stream("2.0.0", &BTreeSet::new(), &mut out)
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND");
    assert!(out.is_empty());
    handle.close().unwrap();
}

#[test]
fn ha14_early_batch_returns_keys_without_root() {
    let f = fixture();
    let root = ObjectKey {
        kind: rdy_store::kinds::TAG,
        hash: f.tag,
    };
    assert_eq!(f.report.header.root(), root);

    // the records that need nothing, split off by the re-exported reader and writer; the root
    // is not among them and is the batch's one prerequisite
    let (mut reader, _) = StreamReader::open(f.bytes.as_slice()).unwrap();
    let mut leaves: Vec<StreamRecord> = Vec::new();
    while let Some(rec) = reader.next().unwrap() {
        if rec.refs().unwrap().is_empty() {
            leaves.push(rec);
        }
    }
    let totals: StreamTotals = reader.finish().unwrap();
    assert_eq!(totals.records, f.report.records);
    assert!(!leaves.is_empty() && leaves.len() < f.report.keys.len());
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        f.repo,
        f.tag,
        vec![root],
    );
    let mut writer = StreamWriter::new(Vec::new(), &header).unwrap();
    for rec in &leaves {
        writer.record(rec).unwrap();
    }
    let (early, _) = writer.finish().unwrap();

    // the check passes without the root, and its keys keep the wire order
    let report = check_batch(early.as_slice(), Some(f.repo)).unwrap();
    let wire: Vec<ObjectKey> = leaves.iter().map(StreamRecord::key).collect();
    assert_eq!(report.keys, wire);
    assert!(!report.keys.contains(&root));
    assert_eq!(report.header.prerequisites, vec![root]);

    // the full batch holds the root, last, and the early keys in its order
    assert_eq!(f.report.keys.last(), Some(&root));
    let in_full: Vec<ObjectKey> = f
        .report
        .keys
        .iter()
        .copied()
        .filter(|k| wire.contains(k))
        .collect();
    assert_eq!(in_full, wire);
}

type WriteFn = fn(&Snapshot, &str, &BTreeSet<ObjectKey>, Vec<u8>) -> Result<BatchReport, RdyError>;
type HasFn = fn(&Snapshot, &[ObjectKey]) -> Result<Vec<ObjectPresence>, RdyError>;
type CheckFn = fn(std::io::Cursor<Vec<u8>>, Option<RepoId>) -> Result<BatchReport, RdyError>;

#[test]
fn ha14_known_uses_exact_object_keys() {
    // the public signatures, by type: object keys in, a report out
    let _write: WriteFn = Snapshot::write_stream::<Vec<u8>>;
    let _has: HasFn = Snapshot::has_objects;
    let _check: CheckFn = check_batch::<std::io::Cursor<Vec<u8>>>;

    let f = fixture();
    let handle = StoreHandle::open(f.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();

    // what the store answers it holds is what an export may leave out
    let asked: Vec<ObjectKey> = f.report.keys.iter().copied().take(3).collect();
    let rows = snap.has_objects(&asked).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.present && !r.redacted));
    let known: BTreeSet<ObjectKey> = rows.iter().filter(|r| r.present).map(|r| r.key).collect();
    let (_, report) = export(&snap, "v1.0.0", &known);
    assert!(report.keys.iter().all(|k| !known.contains(k)));
    assert_eq!(report.keys.len() + 3, f.report.keys.len());

    // a known key must match by kind as well as hash
    let other_kind = ObjectKey {
        kind: if asked[0].kind == rdy_store::kinds::TAG {
            rdy_store::kinds::BLOCK
        } else {
            rdy_store::kinds::TAG
        },
        hash: asked[0].hash,
    };
    assert!(!snap.has_objects(&[other_kind]).unwrap()[0].present);
    let wrong_kind: BTreeSet<ObjectKey> = [other_kind].into();
    let (_, unchanged) = export(&snap, "v1.0.0", &wrong_kind);
    assert_eq!(unchanged.keys, f.report.keys);

    // keys of a successful batch report are the next call's known set
    let first: BTreeSet<ObjectKey> = f.report.keys.iter().copied().take(4).collect();
    let (_, second) = export(&snap, "v1.0.0", &first);
    let mut both: BTreeSet<ObjectKey> = first.clone();
    both.extend(second.keys.iter().copied());
    let (_, third) = export(&snap, "v1.0.0", &both);
    assert!(third.keys.is_empty());
    handle.close().unwrap();
}

#[test]
fn ha14_adapter_keeps_no_quarantine_state() {
    let f = fixture();
    let holder_dir = tempfile::tempdir().unwrap();
    let holder_root = holder_dir.path().join("holder");
    let handle = StoreHandle::create_holder(&holder_root, f.repo).unwrap();
    let keys = f.report.keys.clone();

    // a passing check holds nothing: the holder still lacks every key
    let report = check_batch(f.bytes.as_slice(), Some(f.repo)).unwrap();
    assert_eq!(report.keys, keys);
    let absent = |h: &StoreHandle| {
        h.snapshot()
            .unwrap()
            .has_objects(&keys)
            .unwrap()
            .iter()
            .all(|p| !p.present && !p.redacted)
    };
    assert!(absent(&handle));
    handle.close().unwrap();

    // after a reopen the same calls give the same answers, with no state carried between them
    let handle = StoreHandle::open_holder(&holder_root).unwrap();
    assert!(absent(&handle));
    assert_eq!(check_batch(f.bytes.as_slice(), None).unwrap(), report);
    assert_eq!(
        check_batch(f.bytes.as_slice(), Some(f.repo)).unwrap(),
        report
    );
    assert!(absent(&handle));
    handle.close().unwrap();

    // a failed check leaves nothing either: the intact batch passes at once
    let cut = &f.bytes[..f.bytes.len() - 1];
    assert!(check_batch(cut, None).is_err());
    assert_eq!(check_batch(f.bytes.as_slice(), None).unwrap(), report);
}

/// A consumer that names every type and call from `rdy_adapter` and `std` alone.
mod consumer {
    use rdy_adapter::stream::{
        BatchReport, ObjectKey, StreamError, StreamHeader, StreamPurpose, StreamReader,
        StreamRecord, StreamTotals, StreamWriter, check_batch,
    };
    use rdy_adapter::{Hash, RdyError, RepoId, Snapshot, StoreHandle};
    use std::collections::BTreeSet;
    use std::io::Cursor;
    use std::path::Path;

    /// Exports `tag` from the project at `project`, checks the batch and splits it again.
    pub fn round_trip(project: &Path, tag: &str, known: &[ObjectKey]) -> Result<(), RdyError> {
        let handle = StoreHandle::open(project)?;
        let snap: Snapshot = handle.snapshot()?;
        let known: BTreeSet<ObjectKey> = known.iter().copied().collect();
        let mut out: Vec<u8> = Vec::new();
        let written: BatchReport = snap.write_stream(tag, &known, &mut out)?;
        let checked: BatchReport = check_batch(Cursor::new(&out), Some(written.header.repo_id))?;
        assert_eq!(written, checked);
        let repo: RepoId = checked.header.repo_id;
        let root: Hash = checked.header.tag;
        let purpose: StreamPurpose = checked.header.purpose;
        assert_eq!(purpose, StreamPurpose::TagHistory);
        let (mut reader, header) = reads(&out).map_err(|e| to_rdy(&e))?;
        let header: StreamHeader = header;
        assert_eq!((header.repo_id, header.tag), (repo, root));
        let mut seen: Vec<ObjectKey> = Vec::new();
        let mut writer = StreamWriter::new(Vec::new(), &header).map_err(|e| to_rdy(&e))?;
        while let Some(rec) = reader.next().map_err(|e| to_rdy(&e))? {
            let rec: StreamRecord = rec;
            seen.push(rec.key());
            writer.record(&rec).map_err(|e| to_rdy(&e))?;
        }
        let totals: StreamTotals = reader.finish().map_err(|e| to_rdy(&e))?;
        let (copy, again) = writer.finish().map_err(|e| to_rdy(&e))?;
        assert_eq!((copy, again), (out, totals));
        assert_eq!(seen, checked.keys);
        handle.close()
    }

    fn reads(bytes: &[u8]) -> Result<(StreamReader<&[u8]>, StreamHeader), StreamError> {
        StreamReader::open(bytes)
    }

    fn to_rdy(e: &StreamError) -> RdyError {
        RdyError::new(e.code, e.message.clone(), e.fix.clone())
    }
}

#[test]
fn ha14_public_signatures_need_no_transitive_crate_import() {
    let f = fixture();
    consumer::round_trip(f.dir.path(), "v1.0.0", &[]).unwrap();
    let known: Vec<ObjectKey> = f.report.keys.iter().copied().take(2).collect();
    consumer::round_trip(f.dir.path(), "1.0.0", &known).unwrap();
    assert_eq!(
        consumer::round_trip(f.dir.path(), "9.9.9", &[])
            .unwrap_err()
            .code,
        "E_NOT_FOUND"
    );
}

#[allow(dead_code)]
fn _error_type_is_named(e: StreamError) -> StreamError {
    e
}

// ---------------------------------------------------------------------------------------------
// rf4: a hostile Reflects record (D-4WC6R, tb-A4DAV)

/// postcard's varint of `n`.
fn varint(mut n: u64) -> Vec<u8> {
    let mut out = vec![];
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

/// A closed project whose claim is rewritten once by a typed change justified by a Reflects
/// with one source and code, landed and tagged `v1.0.0`. Returns its directory, repo, the tag's
/// landing, core's export of the tag and the change as stored.
fn reflects_project() -> (tempfile::TempDir, RepoId, Hash, Vec<u8>, rdy_model::Change) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "source", "id": "$s", "parent": "$d", "fields": {"title": "The spec", "kind": "spec", "locator": "spec/v1.md"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$d", "body": "One."}
        ]),
    );
    land(&mut r, "first");
    let claim = rdy_model::BlockId::parse(&w.created["$c"]).unwrap();
    let scope = rdy_engine::Scope::View("work".into());
    let mut next = r.read().block(&scope, &claim).unwrap();
    let base = next.version_hash();
    next.body = "Two.".into();
    let change = rdy_model::Change {
        view: "work".into(),
        ops: vec![rdy_model::Op::Update { base, block: next }],
        deps: r.read().included("work").to_vec(),
        provenance: rdy_model::Provenance {
            actor: "agent:a".into(),
            run: None,
            role: Role::Writer,
            at_ms: 7,
        },
        justification: Some(rdy_model::Justification::Reflects {
            sources: vec![rdy_model::BlockId::parse(&w.created["$s"]).unwrap()],
            code: true,
        }),
        message: "rf4".into(),
        links: Default::default(),
        read_identity: None,
    };
    let id = r.store_mut().commit_to_view("work", change).unwrap();
    r.invalidate();
    // the change as stored (the store fills in its deps)
    let change = r.store().change(&id).unwrap();
    land(&mut r, "reflects");
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let tag = r.store().tags().unwrap()[0].1;
    let landing = r.store().read_tag(&tag).unwrap().landing;
    let repo = r.store().repo_id().unwrap().unwrap();
    let mut bytes = Vec::new();
    stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut bytes).unwrap();
    r.close().unwrap();
    (dir, repo, landing, bytes, change)
}

/// `bytes` with the record of `change` replaced by one holding `payload`, every other record and
/// the header kept, and a trailer of its own.
fn with_payload(bytes: &[u8], change: Hash, payload: &[u8]) -> Vec<u8> {
    let (mut rd, header) = StreamReader::open(bytes).unwrap();
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    let mut found = false;
    while let Some(r) = rd.next().unwrap() {
        if r.key().hash == change {
            found = true;
            w.record(&StreamRecord::Present {
                // the record's own encoding: the payload keeps its version prefix
                encoding: r.raw_kind() >> 7,
                key: ObjectKey {
                    kind: r.key().kind,
                    hash: Hash::of(payload),
                },
                payload: payload.to_vec(),
            })
            .unwrap();
        } else {
            w.record(&r).unwrap();
        }
    }
    assert!(found, "the change is a record of the export");
    w.finish().unwrap().0
}

/// Every segment file of the store at `root`, by name.
fn segments(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn rf4_hostile_reflects_stream_leaves_holder_unchanged() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let (dir, repo, landing, bytes, change) = reflects_project();
    let good = check_batch(bytes.as_slice(), Some(repo)).unwrap();
    let id = change.id();
    let encoded = postcard::to_allocvec(&change).unwrap();
    // the latest encoding: the id covers the version prefix (ok4)
    assert_eq!(rdy_model::encoding::encoded_hash(1, &encoded), id);
    // the justification's bytes: 01 (Some), 02 (Reflects), 01 (one source), ...
    let at = encoded
        .windows(3)
        .rposition(|w| w == [1, 2, 1])
        .expect("the justification");
    // each hostile payload keeps the frame's version prefix
    let mut oversized = vec![1, 0];
    oversized.extend_from_slice(&encoded[..at + 2]);
    oversized.extend(varint(1 << 40));
    let mut truncated = vec![1, 0];
    truncated.extend_from_slice(&encoded[..encoded.len() - 1]);
    let root = dir.path().join("holder");
    StoreHandle::create_holder(&root, repo)
        .unwrap()
        .close()
        .unwrap();
    let before = segments(&root);
    for (name, payload, message) in [
        ("oversized", &oversized, "100000 collection elements"),
        ("truncated", &truncated, "record does not decode"),
    ] {
        let hostile = with_payload(&bytes, id, payload);
        // the check refuses it at the record, through the one bounded decoder
        let e = check_batch(hostile.as_slice(), Some(repo)).unwrap_err();
        assert!(e.message.ends_with(message), "{name}: {e:?}");
        let core = stream_check::check_batch(hostile.as_slice(), Some(repo)).unwrap_err();
        same_error(&e, &core);
        // the take refuses it the same way and appends nothing
        let path = dir.path().join(format!("{name}.rdysync"));
        std::fs::write(&path, &hostile).unwrap();
        let mut h = StoreHandle::open_holder(&root).unwrap();
        let t = h
            .take(
                &[std::fs::File::open(&path).unwrap()],
                rdy_adapter::UploadTarget {
                    tag: "v1.0.0".into(),
                    landing,
                },
            )
            .unwrap_err();
        assert!(t.message.ends_with(message), "{name}: {t:?}");
        let snap = h.snapshot().unwrap();
        assert!(snap.tags().unwrap().is_empty(), "{name}");
        assert!(
            snap.has_objects(&good.keys)
                .unwrap()
                .iter()
                .all(|p| !p.present),
            "{name}: no object of the batch is held"
        );
        drop(snap);
        h.close().unwrap();
        assert_eq!(segments(&root), before, "{name}: the log is unchanged");
    }
}
