//! sdl1 (snapshot deltas, D-RP6SM): kind 30, SNAPSHOT_DELTA, at encoding 1 only; the canonical
//! SNAPSHOT bytes check; the derived `snapshot_deltas` index, built from the delta frames alone;
//! and the hidden thread-local staging guard that registers the kind until its public activation.
use rdy_model::encoding::SNAPSHOT_DELTA_ENCODING;
use rdy_model::{BlockId, Hash, SnapshotDelta, StateSnapshot, diff_snapshots};
use rdy_store::bounded::RecordError;
use rdy_store::codec;
use rdy_store::format::{
    self, FEATURE_SNAPSHOT_DELTA, SNAPSHOT_DELTA_REQUIRED_RDY, stage_snapshot_delta_for_tests,
};
use rdy_store::index::{Index, IndexEntry};
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError, kinds};
use std::collections::BTreeMap;
use std::path::Path;

const TABLE_GONE: &str = "the index has no snapshot delta table; it is rebuilt from the log";

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

fn map(pairs: &[(&str, u8)]) -> BTreeMap<BlockId, Hash> {
    pairs.iter().map(|(i, n)| (id(i), h(*n))).collect()
}

/// Two landings' deltas, chained: the first sets its whole map, the second changes one pair.
fn chain() -> (SnapshotDelta, SnapshotDelta) {
    let m1 = map(&[("A-00001", 1), ("B-00002", 2)]);
    let m2 = map(&[("A-00001", 1), ("B-00002", 3)]);
    let d1 = diff_snapshots(h(10), None, &BTreeMap::new(), &m1);
    let d2 = diff_snapshots(h(11), Some(d1.id()), &m1, &m2);
    (d1, d2)
}

fn loc(offset: u64) -> Location {
    Location { segment: 0, offset }
}

/// A postcard varint of `n`, minimal.
fn varint(mut n: usize) -> Vec<u8> {
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

/// A postcard string with a minimal length varint, or with the length padded to two bytes.
fn text(s: &str, padded: bool) -> Vec<u8> {
    let mut out = if padded {
        vec![(s.len() as u8) | 0x80, 0x00]
    } else {
        varint(s.len())
    };
    out.extend_from_slice(s.as_bytes());
    out
}

#[test]
fn sdl1_delta_encoding_is_one_and_hashes_stored_bytes() {
    assert_eq!(kinds::SNAPSHOT_DELTA, 30);
    let (d1, _) = chain();
    let body = postcard::to_allocvec(&d1).unwrap();
    // without the guard the kind is not registered: no encoding of it splits
    assert_eq!(codec::latest(kinds::SNAPSHOT_DELTA), 0);
    assert_eq!(codec::first(kinds::SNAPSHOT_DELTA), 1, "never encoding 0");
    let mut payload = vec![1u8, 0];
    payload.extend(&body);
    assert_eq!(
        codec::split(30 | 0x80, &payload).unwrap_err(),
        RecordError::UnknownVersion
    );
    assert_eq!(
        codec::split(30, &body).unwrap_err(),
        RecordError::UnknownVersion
    );
    match codec::decode_record::<SnapshotDelta>(158, &payload) {
        Err(StoreError::Format { message, .. }) => assert_eq!(
            message,
            "the store holds an object this rdy cannot read: kind SNAPSHOT_DELTA, encoding 1"
        ),
        other => panic!("an unstaged delta decoded: {other:?}"),
    }

    let _staged = stage_snapshot_delta_for_tests();
    assert_eq!(
        codec::latest(kinds::SNAPSHOT_DELTA),
        SNAPSHOT_DELTA_ENCODING
    );
    let o = codec::encode_new(kinds::SNAPSHOT_DELTA, &d1);
    assert_eq!(o.version, 1);
    assert_eq!(o.raw_kind(), 158, "30 | 0x80");
    assert_eq!(o.payload, payload, "the version prefix, then the body");
    assert_eq!(o.hash(), Hash::of(&payload));
    assert_eq!(o.hash(), d1.id(), "the id hashes the stored bytes");
    assert_ne!(o.hash(), Hash::of(&body));
    let back: SnapshotDelta = codec::decode_record(158, &payload).unwrap();
    assert_eq!(back, d1);
    assert_eq!(
        codec::decode_canonical::<SnapshotDelta>(158, &payload).unwrap(),
        d1
    );
    // encoding 0 is never kind 30's: an unmarked frame, or a marked version 0, does not split
    assert_eq!(
        codec::split(30, &body).unwrap_err(),
        RecordError::UnknownVersion
    );
    let mut zero = vec![0u8, 0];
    zero.extend(&body);
    assert_eq!(
        codec::split(158, &zero).unwrap_err(),
        RecordError::NoncanonicalVersion
    );
    let mut two = vec![2u8, 0];
    two.extend(&body);
    assert_eq!(
        codec::split(158, &two).unwrap_err(),
        RecordError::UnknownVersion
    );
    assert!(codec::frame(kinds::SNAPSHOT_DELTA, 0, body.clone()).is_err());
    // a delta decodes as nothing else, and nothing else decodes as a delta
    assert!(codec::decode_record::<StateSnapshot>(158, &payload).is_err());
    let snap = StateSnapshot {
        blocks: map(&[("A-00001", 1)]),
    };
    let (raw, snap_payload) = codec::item_new(kinds::SNAPSHOT, &snap);
    assert!(codec::decode_record::<SnapshotDelta>(raw, &snap_payload).is_err());
    // the old pins stay: a SNAPSHOT is kind 5 at encoding 0, its bare canonical bytes
    assert_eq!(raw, kinds::SNAPSHOT);
    assert_eq!(snap_payload, snap.canonical_bytes());
    assert_eq!(Hash::of(&snap_payload), snap.id());
    assert_eq!(codec::latest(kinds::SNAPSHOT), 0);

    // a write stores the bytes under that hash and turns the feature on in the same batch
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    assert!(!s.features().has(FEATURE_SNAPSHOT_DELTA));
    let hashes = s.write(vec![o.clone().into_item()]).unwrap();
    assert_eq!(hashes, vec![d1.id()]);
    let f = s
        .features()
        .features
        .iter()
        .find(|f| f.name == FEATURE_SNAPSHOT_DELTA)
        .expect("the first delta turns the feature on")
        .clone();
    assert_eq!(f.class, format::FeatureClass::Incompatible);
    assert_eq!(f.required_rdy, SNAPSHOT_DELTA_REQUIRED_RDY);
    let (raw, stored) = s.reader().get_raw(&d1.id()).unwrap();
    assert_eq!((raw, stored), (158, payload));
}

#[test]
fn sdl1_snapshot_canonical_bytes_reject_decode_aliases() {
    let a = "A-00001";
    let b = "B-00002";
    let snap = StateSnapshot {
        blocks: map(&[(a, 1), (b, 2)]),
    };
    let canonical = snap.canonical_bytes();
    let mut want = vec![2u8];
    want.extend(text(a, false));
    want.extend([1u8; 32]);
    want.extend(text(b, false));
    want.extend([2u8; 32]);
    assert_eq!(canonical, want);
    assert_eq!(
        codec::decode_canonical::<StateSnapshot>(kinds::SNAPSHOT, &canonical).unwrap(),
        snap
    );
    let entry = |k: &str, n: u8, padded: bool| {
        let mut e = text(k, padded);
        e.extend([n; 32]);
        e
    };
    let aliases: Vec<(&str, Vec<u8>)> = vec![
        ("a non-minimal map length", {
            let mut v = vec![0x82, 0x00];
            v.extend(entry(a, 1, false));
            v.extend(entry(b, 2, false));
            v
        }),
        ("a non-minimal key length", {
            let mut v = vec![2u8];
            v.extend(entry(a, 1, true));
            v.extend(entry(b, 2, false));
            v
        }),
        ("keys out of order", {
            let mut v = vec![2u8];
            v.extend(entry(b, 2, false));
            v.extend(entry(a, 1, false));
            v
        }),
        ("a repeated key", {
            let mut v = vec![3u8];
            v.extend(entry(a, 1, false));
            v.extend(entry(a, 1, false));
            v.extend(entry(b, 2, false));
            v
        }),
    ];
    for (what, bytes) in &aliases {
        // postcard alone decodes each alias to the same map, whose id is the canonical one
        let plain: StateSnapshot = codec::decode_record(kinds::SNAPSHOT, bytes).unwrap();
        assert_eq!(plain, snap, "{what}");
        assert_eq!(plain.id(), snap.id(), "{what}: the map hash cannot tell");
        assert_ne!(Hash::of(bytes), snap.id(), "{what}: the stored bytes can");
        // the check compares every byte of a re-encoding
        assert!(!plain.is_encoding_of(bytes), "{what}");
        assert_eq!(
            codec::decode_canonical::<StateSnapshot>(kinds::SNAPSHOT, bytes).unwrap_err(),
            RecordError::Decode,
            "{what}"
        );
    }
    // a repeated key with another version changes the map, and is refused too
    let mut clash = vec![2u8];
    clash.extend(entry(a, 1, false));
    clash.extend(entry(a, 9, false));
    assert!(codec::decode_canonical::<StateSnapshot>(kinds::SNAPSHOT, &clash).is_err());
    // an id that is no block id is no snapshot, Unicode included
    let mut unicode = vec![1u8];
    unicode.extend(entry("\u{00e9}-00001", 1, false));
    assert!(codec::decode_record::<StateSnapshot>(kinds::SNAPSHOT, &unicode).is_err());
    assert!(codec::decode_canonical::<StateSnapshot>(kinds::SNAPSHOT, &unicode).is_err());
    // the empty map and the map length's varint boundary
    assert_eq!(
        codec::decode_canonical::<StateSnapshot>(kinds::SNAPSHOT, &[0]).unwrap(),
        StateSnapshot::default()
    );
    assert!(codec::decode_canonical::<StateSnapshot>(kinds::SNAPSHOT, &[0x80, 0x00]).is_err());
    for n in [127usize, 128] {
        let big = StateSnapshot {
            blocks: (0..n)
                .map(|i| (id(&format!("A-{i:05}")), h(i as u8)))
                .collect(),
        };
        let bytes = big.canonical_bytes();
        assert_eq!(&bytes[..varint(n).len()], &varint(n)[..]);
        assert_eq!(
            codec::decode_canonical::<StateSnapshot>(kinds::SNAPSHOT, &bytes).unwrap(),
            big
        );
    }

    // a delta's bytes are held to the same rule at the same boundary
    let _staged = stage_snapshot_delta_for_tests();
    let (d1, _) = chain();
    let payload = codec::encode_new(kinds::SNAPSHOT_DELTA, &d1).payload;
    // landing (32), base tag (1), full (32): the prefix and the set's length follow
    let at = 2 + 32 + 1 + 32;
    assert_eq!(payload[at], 2, "two set pairs");
    let mut padded = payload[..at].to_vec();
    padded.extend([0x82, 0x00]);
    padded.extend(&payload[at + 1..]);
    let plain: SnapshotDelta = codec::decode_record(158, &padded).unwrap();
    assert_eq!(plain, d1, "postcard alone reads the alias");
    assert_eq!(
        codec::decode_canonical::<SnapshotDelta>(158, &padded).unwrap_err(),
        RecordError::Decode
    );
}

#[test]
fn sdl1_delta_index_rebuilds_from_frames_alone() {
    let _staged = stage_snapshot_delta_for_tests();
    let (d1, d2) = chain();
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("s");
    let mut s = Store::create(&path).unwrap();
    // one batch: the deltas alone, with no landing frame; the frames name their landings
    s.write(vec![
        codec::item_new(kinds::SNAPSHOT_DELTA, &d1),
        codec::item_new(kinds::SNAPSHOT_DELTA, &d2),
    ])
    .unwrap();
    let (got1, at1) = s.snapshot_delta_of(&d1.landing).unwrap().unwrap();
    let (got2, at2) = s.snapshot_delta_of(&d2.landing).unwrap().unwrap();
    assert_eq!((got1, got2), (d1.id(), d2.id()));
    assert!(at1 < at2);
    assert_eq!(s.reader().get_raw(&d1.id()).unwrap().0, 158);
    assert_eq!(s.snapshot_delta_of(&h(99)).unwrap(), None);
    // a write of the same delta again stores nothing more and keeps the row
    s.write(vec![codec::item_new(kinds::SNAPSHOT_DELTA, &d1)])
        .unwrap();
    assert_eq!(
        s.snapshot_delta_of(&d1.landing).unwrap(),
        Some((d1.id(), at1))
    );
    let dump = s.index_dump_for_tests().unwrap();
    assert_eq!(dump.snapshot_deltas.len(), 2);
    let row = &dump.snapshot_deltas[&d1.landing.0.to_vec()];
    assert_eq!(&row[..32], &d1.id().0[..]);
    // a reindex reproduces every table, the capacity counts included
    s.reindex().unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), dump);
    drop(s);

    // an index without the table is refused, and the open rebuilds it from the log
    let file = path.join("index.redb");
    {
        let raw = redb::Database::create(&file).unwrap();
        let w = raw.begin_write().unwrap();
        let def: redb::TableDefinition<&[u8], &[u8]> =
            redb::TableDefinition::new("snapshot_deltas");
        assert!(w.delete_table(def).unwrap(), "the table was there");
        w.commit().unwrap();
    }
    match Index::open(&file) {
        Err(StoreError::Db(m)) => assert_eq!(m, TABLE_GONE),
        Err(e) => panic!("another refusal: {e}"),
        Ok(_) => panic!("an index without snapshot_deltas opened"),
    }
    let s = Store::open(&path).unwrap();
    assert!(s.open_report().index_rebuilt, "a missing table is rebuilt");
    assert_eq!(s.index_dump_for_tests().unwrap(), dump);
    assert_eq!(
        s.snapshot_delta_of(&d2.landing).unwrap(),
        Some((d2.id(), at2))
    );
    drop(s);

    // the index itself: repeated physical copies of one delta keep its first row
    let ix_dir = tempfile::tempdir().unwrap();
    let ix = Index::open(&ix_dir.path().join("index.redb")).unwrap();
    let entry = |delta: &SnapshotDelta, at: u64| IndexEntry::SnapshotDelta {
        landing: delta.landing,
        delta: delta.id(),
        at: loc(at),
    };
    ix.apply(&[entry(&d1, 10), entry(&d1, 20)], loc(30))
        .unwrap();
    ix.apply(&[entry(&d1, 40)], loc(50)).unwrap();
    assert_eq!(
        ix.snapshot_delta(&d1.landing).unwrap(),
        Some((d1.id(), loc(10)))
    );
    assert_eq!(ix.dump().unwrap().snapshot_deltas.len(), 1);
}

#[test]
fn sdl1_delta_index_refuses_distinct_duplicates() {
    let _staged = stage_snapshot_delta_for_tests();
    let (d1, _) = chain();
    // another delta for the same landing: other bytes, another hash
    let other = SnapshotDelta {
        full: h(77),
        ..d1.clone()
    };
    assert_ne!(other.id(), d1.id());
    let refusal = format!(
        "landing {}: expected exactly one snapshot delta",
        d1.landing
    );

    // the index refuses it in one commit, a later one, and keeps what it had
    let ix_dir = tempfile::tempdir().unwrap();
    let ix = Index::open(&ix_dir.path().join("index.redb")).unwrap();
    let entry = |delta: &SnapshotDelta, at: u64| IndexEntry::SnapshotDelta {
        landing: delta.landing,
        delta: delta.id(),
        at: loc(at),
    };
    match ix.apply(&[entry(&d1, 10), entry(&other, 20)], loc(30)) {
        Err(StoreError::Decode(m)) => assert_eq!(m, refusal),
        other => panic!("two deltas of one landing were indexed: {other:?}"),
    }
    assert_eq!(
        ix.snapshot_delta(&d1.landing).unwrap(),
        None,
        "nothing applied"
    );
    ix.apply(&[entry(&d1, 10)], loc(30)).unwrap();
    match ix.apply(&[entry(&other, 40)], loc(50)) {
        Err(StoreError::Decode(m)) => assert_eq!(m, refusal),
        other => panic!("a second delta of one landing was indexed: {other:?}"),
    }
    assert_eq!(
        ix.snapshot_delta(&d1.landing).unwrap(),
        Some((d1.id(), loc(10)))
    );

    // a store write refuses it, in its own batch or a later one, and appends nothing
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let end = s.log_end();
    match s.write(vec![
        codec::item_new(kinds::SNAPSHOT_DELTA, &d1),
        codec::item_new(kinds::SNAPSHOT_DELTA, &other),
    ]) {
        Err(StoreError::Decode(m)) => assert_eq!(m, refusal),
        other => panic!("a batch with two deltas of one landing was written: {other:?}"),
    }
    assert_eq!(s.log_end(), end, "the batch rolled back");
    s.write(vec![codec::item_new(kinds::SNAPSHOT_DELTA, &d1)])
        .unwrap();
    let end = s.log_end();
    match s.write(vec![codec::item_new(kinds::SNAPSHOT_DELTA, &other)]) {
        Err(StoreError::Decode(m)) => assert_eq!(m, refusal),
        other => panic!("a second delta of one landing was written: {other:?}"),
    }
    assert_eq!(s.log_end(), end, "the write rolled back");
    assert!(!s.has(&other.id()).unwrap());
    let dump = s.index_dump_for_tests().unwrap();
    drop(s);
    let s = Store::open(&path).unwrap();
    assert_eq!(
        s.snapshot_delta_of(&d1.landing).unwrap().unwrap().0,
        d1.id()
    );
    assert_eq!(s.index_dump_for_tests().unwrap(), dump);
    drop(s);
    check_closed(&path);
}

/// The store at `path` opens and its log verifies.
fn check_closed(path: &Path) {
    let s = Store::open(path).unwrap();
    s.verify().unwrap();
}

#[test]
fn sdl1_staging_guard_is_thread_local_and_restores_registries() {
    let (d1, _) = chain();
    let body = postcard::to_allocvec(&d1).unwrap();
    let mut payload = vec![1u8, 0];
    payload.extend(&body);
    let names = || -> Vec<String> {
        format::registered_features()
            .into_iter()
            .map(|f| f.name)
            .collect()
    };
    let off = |when: &str| {
        assert_eq!(names().len(), 9, "{when}");
        assert!(
            !names().contains(&FEATURE_SNAPSHOT_DELTA.to_string()),
            "{when}"
        );
        assert!(
            format::required_features(158, &payload).is_empty(),
            "{when}"
        );
        assert_eq!(codec::latest(kinds::SNAPSHOT_DELTA), 0, "{when}");
        assert!(codec::split(158, &payload).is_err(), "{when}");
        assert!(!rdy_store::sync_stream::is_sync_kind(30), "{when}");
        assert!(
            !rdy_store::sync_stream::sync_kinds().contains(&30),
            "{when}"
        );
        assert_eq!(rdy_store::sync_stream::sync_kinds().len(), 23, "{when}");
        assert_eq!(
            rdy_store::dump::feature_frame_kinds(),
            &rdy_store::dump::FEATURE_FRAME_KINDS[..],
            "{when}"
        );
        let unknown = format::StoreFeatures::of(vec![format::StoreFeature {
            name: FEATURE_SNAPSHOT_DELTA.into(),
            class: format::FeatureClass::Incompatible,
            required_rdy: SNAPSHOT_DELTA_REQUIRED_RDY.into(),
        }]);
        match format::check_features(&unknown) {
            Err(StoreError::Format { message, .. }) => assert_eq!(
                message,
                format!("store feature snapshot_delta needs rdy {SNAPSHOT_DELTA_REQUIRED_RDY}"),
                "{when}"
            ),
            other => panic!("{when}: an unstaged rdy knows snapshot_delta: {other:?}"),
        }
    };
    let on = || {
        assert_eq!(names().len(), 10);
        assert!(names().contains(&FEATURE_SNAPSHOT_DELTA.to_string()));
        let used: Vec<String> = format::required_features(158, &payload)
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(
            used,
            vec![
                format::FEATURE_OBJECT_ENCODING.to_string(),
                FEATURE_SNAPSHOT_DELTA.to_string()
            ]
        );
        assert_eq!(codec::latest(kinds::SNAPSHOT_DELTA), 1);
        assert_eq!(codec::split(158, &payload).unwrap().kind, 30);
        assert!(rdy_store::sync_stream::is_sync_kind(30));
        assert_eq!(rdy_store::sync_stream::sync_kinds().len(), 24);
        assert_eq!(rdy_store::sync_stream::sync_kinds().last(), Some(&30));
        assert!(
            rdy_store::dump::feature_frame_kinds()
                .contains(&(FEATURE_SNAPSHOT_DELTA, kinds::SNAPSHOT_DELTA))
        );
        let manifest = format::baseline_features().merged(&[format::StoreFeature {
            name: FEATURE_SNAPSHOT_DELTA.into(),
            class: format::FeatureClass::Incompatible,
            required_rdy: SNAPSHOT_DELTA_REQUIRED_RDY.into(),
        }]);
        assert_eq!(
            format::check_features(&manifest).unwrap(),
            format::StoreAccess::ReadWrite
        );
    };
    off("before");
    {
        let _outer = stage_snapshot_delta_for_tests();
        on();
        // another thread never sees this thread's guard
        std::thread::scope(|sc| sc.spawn(|| off("on another thread")).join().unwrap());
        {
            let _inner = stage_snapshot_delta_for_tests();
            on();
        }
        // the inner guard's drop keeps the outer one
        on();
    }
    off("after");
    // the baseline a new store starts with is unchanged by the guard
    let _staged = stage_snapshot_delta_for_tests();
    let base: Vec<String> = format::baseline_features()
        .features
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(base, vec!["content_hash_v2", "object_encoding"]);
}
