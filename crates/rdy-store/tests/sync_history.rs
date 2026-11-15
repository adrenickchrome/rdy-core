//! scc2 (D-KE83X, D-4XHPH, D-WWAK7): sync stream format 4. Purposes 2 `canon_history` (root a
//! LANDING) and 3 `view_history` (root a VIEW_OP); older formats keep their purposes; the
//! closure of a history keeps late attachments below its base and the records a view op names;
//! the header lists only the features its records need; a minimum is the numeric maximum of the
//! incompatible and read-only features; hostile headers and closures stay bounded.
#[path = "support/carried.rs"]
mod carried_fixture;

use carried_fixture::copy_store;
use rdy_model::signature::Signature;
use rdy_model::*;
use rdy_store::closure::{
    END_OF_LOG, StoreSource, closure_before, history_closure_before, write_batch,
};
use rdy_store::format::{
    FEATURE_SNAPSHOT_DELTA, FEATURE_SYNC_STATE, FeatureClass, RdyVersion, StoreFeature,
    StoreFeatures, required_features,
};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_state::{SyncCanon, SyncStep};
use rdy_store::sync_stream::{
    BatchReport, HISTORY_FORMAT, READ_FORMATS, STREAM_FORMAT, StreamHeader, StreamPurpose,
    StreamReader, StreamRecord, StreamWriter, check_batch,
};
use rdy_store::take_batch::{FetchTarget, TakeError, UploadTarget};
use rdy_store::{LandRequest, Store, StoreReader};
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::path::{Path, PathBuf};

const PRELUDE: usize = 12;
const URL: &str = "https://127.0.0.1:9/r/demo";

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: if role == Role::Approver {
            "human:a".into()
        } else {
            "agent:w".into()
        },
        run: None,
        role,
        at_ms: 1,
    }
}

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

fn block(n: u32) -> Block {
    Block {
        id: BlockId::parse(&format!("b-{n:05}")).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: format!("claim {n}"),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn change(view: &str, n: u32) -> Change {
    Change {
        view: view.into(),
        ops: vec![Op::Create { block: block(n) }],
        deps: vec![],
        provenance: prov(Role::Writer),
        justification: None,
        message: format!("change {n}"),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn sig(subject: Hash, namespace: &str, n: u8) -> Signature {
    Signature {
        subject,
        namespace: namespace.into(),
        statement: format!("statement {n}").into_bytes(),
        sig: vec![n],
        signed_by: prov(Role::Approver),
    }
}

/// Lands one change that creates block `n` through the store's own landing path.
fn land(s: &mut Store, n: u32) -> Hash {
    s.commit_to_view("work", change("work", n)).unwrap();
    let p = s
        .propose("work", &format!("p{n}"), prov(Role::Writer))
        .unwrap();
    s.land_atomic(LandRequest {
        proposal: p,
        accept: None,
        approver: prov(Role::Approver),
        alt: None,
        relay: None,
        evidence: None,
        assessment: None,
    })
    .unwrap()
    .landing
}

/// A project with three landings.
fn history() -> (tempfile::TempDir, PathBuf, Store, [Hash; 3]) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("p");
    let mut s = Store::create(&path).unwrap();
    let ls = [land(&mut s, 1), land(&mut s, 2), land(&mut s, 3)];
    (d, path, s, ls)
}

/// The format-4 batch of `purpose` from `root`, less `known` and `base`'s link closure.
fn export(
    reader: &StoreReader,
    purpose: StreamPurpose,
    root: Hash,
    base: Option<Hash>,
    known: &BTreeSet<ObjectKey>,
) -> Result<(Vec<u8>, StreamHeader, Vec<ObjectKey>), StreamError> {
    let found = history_closure_before(reader, purpose, root, known, base, END_OF_LOG)?;
    let header = match purpose {
        StreamPurpose::ViewHistory => StreamHeader::view_history(
            found.features,
            reader.repo_id().unwrap().unwrap(),
            root,
            found.prerequisites,
        ),
        _ => StreamHeader::canon_history(
            found.features,
            reader.repo_id().unwrap().unwrap(),
            root,
            found.prerequisites,
        ),
    };
    let mut source = StoreSource {
        reader,
        end: END_OF_LOG,
    };
    let (bytes, _) = write_batch(&mut source, &header, &found.records, Vec::new())?;
    Ok((bytes, header, found.records))
}

/// `bytes` with its prelude's stream format set to `format`.
fn with_format(bytes: &[u8], format: u32) -> Vec<u8> {
    let mut b = bytes.to_vec();
    b[8..12].copy_from_slice(&format.to_le_bytes());
    b
}

/// `bytes` with its header payload changed by `f`, and the header frame's length and hash made
/// to agree with it (the rest of the batch is left as it was).
fn reheaded(bytes: &[u8], f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let len = u32::from_le_bytes(bytes[PRELUDE..PRELUDE + 4].try_into().unwrap()) as usize;
    let start = PRELUDE + 37;
    let mut payload = bytes[start..start + len].to_vec();
    f(&mut payload);
    let mut out = bytes[..PRELUDE].to_vec();
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.push(bytes[PRELUDE + 4]);
    out.extend_from_slice(&Hash::of(&payload).0);
    out.extend_from_slice(&payload);
    out.extend_from_slice(&bytes[start + len..]);
    out
}

/// The offset in a header payload of its purpose byte.
fn purpose_at(payload: &[u8]) -> usize {
    let manifest = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
    manifest + 20
}

fn code_and_message(e: StreamError) -> (&'static str, String) {
    (e.code, e.message)
}

fn purpose_refusal(actual: u8, expected: &str) -> (&'static str, String) {
    (
        "E_STREAM_PURPOSE",
        format!("stream purpose {actual} does not match {expected}"),
    )
}

#[test]
fn scc2_format4_roots_and_old_formats_are_exact() {
    assert_eq!(
        STREAM_FORMAT, 2,
        "a tag history is still written in format 2"
    );
    assert_eq!(HISTORY_FORMAT, 4);
    assert_eq!(READ_FORMATS, [2, 3, 4]);
    for (byte, purpose, name, first, root) in [
        (0, StreamPurpose::TagHistory, "tag_history", 2, kinds::TAG),
        (
            1,
            StreamPurpose::LinkDelivery,
            "link_delivery",
            3,
            kinds::BLOCK,
        ),
        (
            2,
            StreamPurpose::CanonHistory,
            "canon_history",
            4,
            kinds::LANDING,
        ),
        (
            3,
            StreamPurpose::ViewHistory,
            "view_history",
            4,
            kinds::VIEW_OP,
        ),
    ] {
        assert_eq!(StreamPurpose::of(byte), Some(purpose));
        assert_eq!(purpose.byte(), byte);
        assert_eq!(purpose.name(), name);
        assert_eq!(purpose.first_format(), first);
        if purpose != StreamPurpose::LinkDelivery {
            assert_eq!(purpose.root_kind(), root, "{name}");
        }
    }
    assert_eq!(StreamPurpose::of(4), None);

    let (_d, _p, s, [l1, _, l3]) = history();
    let reader = s.reader();
    let none = BTreeSet::new();
    // canon history: format 4, purpose 2, a landing root in the header's root field
    let (canon, header, records) =
        export(&reader, StreamPurpose::CanonHistory, l3, None, &none).unwrap();
    assert_eq!(&canon[8..12], &4u32.to_le_bytes());
    assert_eq!(header.stream_format, 4);
    assert_eq!(header.purpose, StreamPurpose::CanonHistory);
    assert_eq!(header.tag, l3);
    assert_eq!(header.root(), key(kinds::LANDING, l3));
    assert!(records.contains(&key(kinds::LANDING, l1)));
    let report: BatchReport = check_batch(&canon[..]).unwrap();
    assert_eq!(report.header, header);
    assert_eq!(report.keys, records);
    let (_, read) = StreamReader::open_history(&canon[..]).unwrap();
    assert_eq!(read, header);
    assert_eq!(
        code_and_message(
            StreamReader::open_for(&canon[..], StreamPurpose::TagHistory)
                .err()
                .unwrap()
        ),
        purpose_refusal(2, "tag_history")
    );
    // view history: purpose 3, a view op root
    let head = reader.view_head("work").unwrap().unwrap().head;
    let (view, vh, _) = export(&reader, StreamPurpose::ViewHistory, head, None, &none).unwrap();
    assert_eq!(vh.stream_format, 4);
    assert_eq!(vh.purpose, StreamPurpose::ViewHistory);
    assert_eq!(vh.root(), key(kinds::VIEW_OP, head));
    assert_eq!(check_batch(&view[..]).unwrap().header, vh);
    // a tag history keeps format 2 and its header's tag root
    let tag = Tag {
        name: "v1".into(),
        landing: l3,
        message: None,
        tagged_by: prov(Role::Approver),
    };
    drop(reader);
    let mut s = s;
    let t = s.put_tag(&tag).unwrap();
    let reader = s.reader();
    let found = closure_before(&reader, t, &none, END_OF_LOG).unwrap();
    let th = StreamHeader::tag_history(
        reader.store_features().unwrap(),
        reader.repo_id().unwrap().unwrap(),
        t,
        found.prerequisites,
    );
    assert_eq!(th.stream_format, 2);
    assert_eq!(th.root(), key(kinds::TAG, t));
    // a format-4 purpose in an older format is E_STREAM_PURPOSE
    for (bytes, format, byte) in [(&canon, 2, 2), (&canon, 3, 2), (&view, 2, 3), (&view, 3, 3)] {
        assert_eq!(
            code_and_message(check_batch(&with_format(bytes, format)[..]).unwrap_err()),
            purpose_refusal(byte, "tag_history"),
            "format {format}, purpose {byte}"
        );
    }
    // and the writer refuses to write one
    let old = StreamHeader {
        stream_format: 2,
        ..header.clone()
    };
    assert_eq!(
        StreamWriter::new(Vec::new(), &old)
            .err()
            .map(code_and_message),
        Some(purpose_refusal(2, "tag_history"))
    );
    // a format no rdy reads is a bad header
    assert_eq!(
        check_batch(&with_format(&canon, 5)[..]).unwrap_err().code,
        "E_BAD_RECORD"
    );
    // the frozen fixtures read as they were written
    check_sync4_fixtures();
}

/// The format-4 fixtures, frozen once (written by `scc2_write_sync4_fixtures_once` in the
/// engine's `sync_fetch` tests, rdy 0.53.0): each file's length and blake3.
const SYNC4_FIXTURES: [(&str, StreamPurpose, [(&str, usize, &str); 2]); 2] = [
    (
        "sync4-canon-history",
        StreamPurpose::CanonHistory,
        [
            (
                "bin",
                5884,
                "8d51365e1d9bd84bf9c00526e672d401298d0128cdf3590d1c09f50eb5f746e4",
            ),
            (
                "expect.json",
                4641,
                "19bd7fd90178c3883d3d9cd018577877a7aaa6eadfb6d1da99355a9693574e5c",
            ),
        ],
    ),
    (
        "sync4-view-history",
        StreamPurpose::ViewHistory,
        [
            (
                "bin",
                6699,
                "0abb9333ec131aa4f87c78e0a549322d053c49048c0c7ab15dd309ec96f7dace",
            ),
            (
                "expect.json",
                5338,
                "3c4a8147ee8439998f41895ed2905165f7f3461dac089e8069b7e7dfa326eff4",
            ),
        ],
    ),
];

/// The format-4 fixtures read as they were written: each file's bytes, its batch's check report
/// and minimum as its expect file records them, and the fetch that took it moved no canon or
/// view ref and listed `sync_state` in the receiver.
fn check_sync4_fixtures() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdy-qa/fixtures/sync");
    for (name, purpose, files) in SYNC4_FIXTURES {
        for (ext, len, digest) in files {
            let b = std::fs::read(dir.join(format!("{name}.{ext}"))).unwrap();
            assert_eq!(
                (b.len(), Hash::of(&b).to_string()),
                (len, digest.to_string()),
                "{name}.{ext}"
            );
        }
        let bin = std::fs::read(dir.join(format!("{name}.bin"))).unwrap();
        let expect: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.join(format!("{name}.expect.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(&bin[8..12], &4u32.to_le_bytes(), "{name}");
        let report = check_batch(&bin[..]).unwrap();
        assert_eq!(report.header.purpose, purpose);
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            expect["stream"],
            "{name}"
        );
        assert_eq!(expect["rdy"], "0.53.0");
        assert_eq!(expect["stream_format"], 4);
        assert_eq!(expect["purpose"], purpose.name());
        assert_eq!(
            serde_json::to_value(report.header.features.min_rdy()).unwrap(),
            expect["upload_min_rdy"]
        );
        assert_eq!(expect["fetch"]["canon_moved"], false);
        assert_eq!(expect["fetch"]["view_moved"], false);
        assert_eq!(expect["fetch"]["min_rdy_after"], "0.53.0");
        let receiver: StoreFeatures =
            serde_json::from_value(expect["receiver_features"].clone()).unwrap();
        assert!(receiver.has(FEATURE_SYNC_STATE), "{name}");
        assert!(!report.header.features.has(FEATURE_SYNC_STATE), "{name}");
    }
}

#[test]
fn scc2_closure_keeps_late_attachments_below_base() {
    let (_d, _p, mut s, [l1, l2, l3]) = history();
    // after the third landing: a signature of the first, a tag of it and the tag's signature
    let late = s.put_signature(&sig(l1, "rdy-approval-v1", 1)).unwrap();
    let tag = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing: l1,
            message: None,
            tagged_by: prov(Role::Approver),
        })
        .unwrap();
    let tag_sig = s.put_signature(&sig(tag, "rdy-tag-v1", 2)).unwrap();
    let reader = s.reader();
    let none = BTreeSet::new();
    let (bytes, header, records) =
        export(&reader, StreamPurpose::CanonHistory, l3, Some(l2), &none).unwrap();
    let has = |k: ObjectKey| records.contains(&k);
    // the base's link closure stays home
    assert!(!has(key(kinds::LANDING, l1)));
    assert!(!has(key(kinds::LANDING, l2)));
    assert!(has(key(kinds::LANDING, l3)));
    // each landing that travels carries its one snapshot delta
    let deltas = records
        .iter()
        .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
        .count();
    assert_eq!(deltas, 1);
    // attachments made later travel, even of records below the base
    assert!(has(key(kinds::SIGNATURE, late)));
    assert!(has(key(kinds::TAG, tag)));
    assert!(has(key(kinds::SIGNATURE, tag_sig)));
    // and what they need from below the base is named, not sent
    assert!(header.prerequisites.contains(&key(kinds::LANDING, l2)));
    assert!(header.prerequisites.contains(&key(kinds::LANDING, l1)));
    check_batch(&bytes[..]).unwrap();
    // with no base the whole chain travels
    let (_, _, all) = export(&reader, StreamPurpose::CanonHistory, l3, None, &none).unwrap();
    for l in [l1, l2, l3] {
        assert!(all.contains(&key(kinds::LANDING, l)));
    }
    assert_eq!(
        all.iter()
            .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
            .count(),
        3
    );
    // a root or base the store does not hold is E_UNRESOLVED_REF
    for (root, base) in [(Hash([5; 32]), None), (l3, Some(Hash([5; 32])))] {
        assert_eq!(
            export(&reader, StreamPurpose::CanonHistory, root, base, &none)
                .unwrap_err()
                .code,
            "E_UNRESOLVED_REF"
        );
    }
}

#[test]
fn scc2_view_closure_requires_named_records() {
    let (_d, _p, mut s, [l1, _, _]) = history();
    s.view_create("draft", Some(l1), None, false, &prov(Role::Writer))
        .unwrap();
    let c1 = s.commit_to_view("draft", change("draft", 21)).unwrap();
    let c2 = s.commit_to_view("draft", change("draft", 22)).unwrap();
    let reader = s.reader();
    let mut ops = vec![];
    let mut at = Some(reader.view_head("draft").unwrap().unwrap().head);
    while let Some(h) = at {
        ops.push(h);
        at = reader.view_op(&h).unwrap().prev;
    }
    ops.reverse();
    let none = BTreeSet::new();
    let (bytes, header, records) = export(
        &reader,
        StreamPurpose::ViewHistory,
        ops[ops.len() - 1],
        None,
        &none,
    )
    .unwrap();
    // every op of the chain, each change it commits, and the landing its create names
    for op in &ops {
        assert!(records.contains(&key(kinds::VIEW_OP, *op)));
    }
    for c in [c1, c2] {
        assert!(records.contains(&key(kinds::CHANGE, c)));
    }
    assert!(records.contains(&key(kinds::LANDING, l1)));
    assert!(header.prerequisites.is_empty());
    check_batch(&bytes[..]).unwrap();
    // above a base op, the base's chain stays home and is named
    let base = ops[ops.len() - 2];
    let (_, above, kept) = export(
        &reader,
        StreamPurpose::ViewHistory,
        ops[ops.len() - 1],
        Some(base),
        &none,
    )
    .unwrap();
    assert!(!kept.contains(&key(kinds::VIEW_OP, base)));
    assert!(!kept.contains(&key(kinds::CHANGE, c1)));
    assert!(kept.contains(&key(kinds::CHANGE, c2)));
    assert!(above.prerequisites.contains(&key(kinds::VIEW_OP, base)));
    // a batch without a record an op names is refused
    let without: Vec<ObjectKey> = records
        .iter()
        .copied()
        .filter(|k| *k != key(kinds::CHANGE, c2))
        .collect();
    let mut source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    let (broken, _) = write_batch(&mut source, &header, &without, Vec::new()).unwrap();
    let e = check_batch(&broken[..]).unwrap_err();
    assert_eq!(e.code, "E_BAD_RECORD");
    assert!(
        e.message
            .ends_with(&format!("unlisted dependency {}", key(kinds::CHANGE, c2))),
        "{}",
        e.message
    );
}

#[test]
fn scc2_upload_features_are_needed_features_only() {
    let (_d, _p, mut s, [l1, _, l3]) = history();
    // the source store lists a feature no record needs
    let r = s.reader().repo_id().unwrap().unwrap();
    s.write_sync(
        "origin",
        vec![SyncStep::Canon(SyncCanon {
            repo_id: r,
            url: URL.into(),
            fetched: Some(l1),
            applied: None,
            held: None,
        })],
    )
    .unwrap();
    let reader = s.reader();
    let store = reader.store_features().unwrap();
    assert!(store.has(FEATURE_SYNC_STATE));
    let none = BTreeSet::new();
    let (bytes, header, _) = export(&reader, StreamPurpose::CanonHistory, l3, None, &none).unwrap();
    // the header names exactly the features the batch's records need
    let (mut r2, _) = StreamReader::open(&bytes[..]).unwrap();
    let mut needs = vec![];
    while let Some(rec) = r2.next().unwrap() {
        if let StreamRecord::Present { payload, .. } = &rec {
            needs.extend(required_features(rec.raw_kind(), payload));
        }
    }
    let needs: BTreeMap<String, StoreFeature> =
        needs.into_iter().map(|f| (f.name.clone(), f)).collect();
    assert_eq!(
        header.features,
        StoreFeatures::of(needs.into_values().collect())
    );
    assert!(!header.features.has(FEATURE_SYNC_STATE));
    assert!(header.features.has(FEATURE_SNAPSHOT_DELTA));
    assert!(header.features.is_subset_of(&store));
    // the same root gives the same list on every export
    let (_, again, _) = export(&reader, StreamPurpose::CanonHistory, l3, None, &none).unwrap();
    assert_eq!(again.features, header.features);
    // the upload's minimum is its needs', not the store's
    assert_eq!(header.features.min_rdy(), Some(RdyVersion(0, 52, 0)));
    assert_eq!(store.min_rdy(), Some(RdyVersion(0, 53, 0)));
    // a tag history still names the source store's whole manifest
    drop(reader);
    let t = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing: l3,
            message: None,
            tagged_by: prov(Role::Approver),
        })
        .unwrap();
    let reader = s.reader();
    let found = closure_before(&reader, t, &none, END_OF_LOG).unwrap();
    let th = StreamHeader::tag_history(
        reader.store_features().unwrap(),
        reader.repo_id().unwrap().unwrap(),
        t,
        found.prerequisites,
    );
    assert!(th.features.has(FEATURE_SYNC_STATE));
}

fn feature(name: &str, class: FeatureClass, rdy: &str) -> StoreFeature {
    StoreFeature {
        name: name.into(),
        class,
        required_rdy: rdy.into(),
    }
}

#[test]
fn scc2_minimum_ignores_compatible_and_compares_numeric_versions() {
    use FeatureClass::*;
    assert_eq!(StoreFeatures::of(vec![]).min_rdy(), None);
    // compatible features never raise it
    let compatible = StoreFeatures::of(vec![feature("zz_later", Compatible, "9.0.0")]);
    assert_eq!(compatible.min_rdy(), None);
    // numeric, not text: 0.10.0 is after 0.9.0, and 0.100.0 after both
    let f = StoreFeatures::of(vec![
        feature("a_old", Incompatible, "0.9.0"),
        feature("b_read", ReadOnlyCompatible, "0.10.0"),
        feature("c_new", Compatible, "9.9.9"),
    ]);
    assert_eq!(f.min_rdy(), Some(RdyVersion(0, 10, 0)));
    assert_eq!(f.min_rdy().unwrap().to_string(), "0.10.0");
    let more = StoreFeatures::of(vec![feature("d_unknown", Incompatible, "0.100.0")]);
    // the minimum counts a feature this rdy does not know: it is not whether it opens
    assert_eq!(more.min_rdy(), Some(RdyVersion(0, 100, 0)));
    // the prospective merged minimum, and the store's own one left as it is
    assert_eq!(f.merged_min_rdy(&more), Some(RdyVersion(0, 100, 0)));
    assert_eq!(f.merged_min_rdy(&compatible), Some(RdyVersion(0, 10, 0)));
    assert_eq!(f.min_rdy(), Some(RdyVersion(0, 10, 0)));
    assert!(RdyVersion(1, 0, 0) > RdyVersion(0, 99, 99));
    assert_eq!(RdyVersion::parse("0.53.0"), Some(RdyVersion(0, 53, 0)));
    assert_eq!(RdyVersion::parse("0.53"), None);
    assert_eq!(RdyVersion::parse("v0.53.0"), None);
    // a new store's own minimum: its baseline features
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("p")).unwrap();
    assert_eq!(s.features().min_rdy(), Some(RdyVersion(0, 52, 0)));
}

/// `bytes` as one batch file.
fn file(dir: &Path, name: &str, bytes: &[u8]) -> std::fs::File {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    std::fs::File::open(p).unwrap()
}

#[test]
fn scc2_hostile_headers_and_closures_stay_bounded() {
    let (d, dir, mut s, [l1, _, l3]) = history();
    let reader = s.reader();
    let none = BTreeSet::new();
    let (canon, header, records) =
        export(&reader, StreamPurpose::CanonHistory, l3, Some(l1), &none).unwrap();
    // an unknown purpose byte, and a prerequisite count past its bound, refused at the header
    let unknown = reheaded(&canon, |p| {
        let at = purpose_at(p);
        p[at] = 9;
    });
    assert_eq!(
        code_and_message(check_batch(&unknown[..]).unwrap_err()),
        purpose_refusal(9, "tag_history")
    );
    let many = reheaded(&canon, |p| {
        let at = purpose_at(p) + 33;
        p[at..at + 4].copy_from_slice(&100_001u32.to_le_bytes());
    });
    let e = check_batch(&many[..]).unwrap_err();
    assert_eq!(e.code, "E_LIMIT");
    assert!(e.message.contains("100000 prerequisites"), "{}", e.message);
    // a history's purpose given to the tag-history closure reader, and a tag purpose to the
    // history closure, are refused
    assert_eq!(
        code_and_message(
            history_closure_before(
                &reader,
                StreamPurpose::TagHistory,
                l3,
                &none,
                None,
                END_OF_LOG
            )
            .err()
            .unwrap()
        ),
        purpose_refusal(0, "canon_history")
    );
    // an unrelated record in the batch is outside the sync closure
    drop(reader);
    let stray = s
        .put(
            kinds::INTENT,
            &Intent {
                stable: None,
                prev: None,
                actor: "agent:w".into(),
                run: None,
                view: "work".into(),
                area: None,
                blocks: vec![],
                angle: "unrelated".into(),
                expires_at_ms: 100,
                status: IntentStatus::Open,
                at_ms: 1,
            },
        )
        .unwrap();
    let reader = s.reader();
    let mut padded = records.clone();
    padded.push(key(kinds::INTENT, stray));
    let mut source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    let (outside, _) = write_batch(&mut source, &header, &padded, Vec::new()).unwrap();
    drop(reader);
    drop(s);
    // the receiver: a copy of the project as it stood (it holds every landing already, so the
    // fetch below adds only what the batch brings)
    let target = d.path().join("q");
    copy_store(&dir, &target);
    let mut q = Store::open(&target).unwrap();
    let fetch = FetchTarget {
        remote: "origin".into(),
        url: URL.into(),
        purpose: StreamPurpose::CanonHistory,
        root: l3,
        view: None,
    };
    let refused = |e: TakeError<Infallible>| match e {
        TakeError::Stream(e) => (e.code, e.message),
        other => panic!("{other}"),
    };
    let end = q.log_end();
    let (code, message) = refused(
        q.take_fetched::<Infallible>(&[file(d.path(), "o", &outside)], &fetch, |_, _| Ok(()))
            .unwrap_err(),
    );
    assert_eq!(code, "E_BAD_RECORD");
    assert!(
        message.ends_with("object is outside the sync closure"),
        "{message}"
    );
    // a root that is not the target's
    let other = FetchTarget {
        root: l1,
        ..fetch.clone()
    };
    let (code, message) = refused(
        q.take_fetched::<Infallible>(&[file(d.path(), "c", &canon)], &other, |_, _| Ok(()))
            .unwrap_err(),
    );
    assert_eq!(code, "E_BAD_RECORD");
    assert_eq!(message, "fetch target does not match the batch root");
    // a view history offered as a canon fetch
    let view = FetchTarget {
        purpose: StreamPurpose::ViewHistory,
        ..fetch.clone()
    };
    assert_eq!(
        q.take_fetched::<Infallible>(&[file(d.path(), "c2", &canon)], &view, |_, _| Ok(()))
            .err()
            .map(refused),
        Some(purpose_refusal(2, "view_history"))
    );
    assert_eq!(q.log_end(), end, "a refused fetch writes nothing");
    // a holder takes no history, and fetches nothing
    let hd = tempfile::tempdir().unwrap();
    let mut h = Store::create_holder(&hd.path().join("h"), header.repo_id).unwrap();
    let upload = UploadTarget {
        tag: "v1".into(),
        landing: l3,
    };
    assert_eq!(
        h.take_batches::<Infallible>(&[file(d.path(), "c3", &canon)], &upload, |_, _| {
            Ok(rdy_store::take_batch::TagChoice::Incoming)
        })
        .err()
        .map(refused),
        Some(purpose_refusal(2, "tag_history"))
    );
    assert!(matches!(
        h.take_fetched::<Infallible>(&[file(d.path(), "c4", &canon)], &fetch, |_, _| Ok(())),
        Err(TakeError::ModeMismatch)
    ));
    // the clean batch is taken
    q.take_fetched::<Infallible>(&[file(d.path(), "c5", &canon)], &fetch, |_, _| Ok(()))
        .unwrap();
}
