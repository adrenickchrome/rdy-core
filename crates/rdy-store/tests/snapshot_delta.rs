//! sdl1 (snapshot deltas, D-RP6SM): kind 30, SNAPSHOT_DELTA, at encoding 1 only; the canonical
//! SNAPSHOT bytes check; the derived `snapshot_deltas` index, built from the delta frames alone;
//! and the hidden thread-local staging guard that registers the kind until its public activation.
//!
//! sdl2: the one map read (`rdy_store::snapshot`): a held SNAPSHOT first, else the fold of the
//! deltas from the nearest held or cached map; its bounded cache; and its refusals.
use rdy_model::encoding::SNAPSHOT_DELTA_ENCODING;
use rdy_model::{
    Assurance, BlockId, Hash, LandingRecord, Provenance, Role, SnapshotDelta, StateSnapshot,
    diff_snapshots, snapshot_map_id,
};
use rdy_store::bounded::RecordError;
use rdy_store::codec;
use rdy_store::format::{
    self, FEATURE_SNAPSHOT_DELTA, SNAPSHOT_DELTA_REQUIRED_RDY, stage_snapshot_delta_for_tests,
};
use rdy_store::index::{Index, IndexEntry};
use rdy_store::object_refs::ObjectKey;
use rdy_store::segment::Location;
use rdy_store::snapshot::{
    CacheTag, MapCache, RecordSnapshots, folded_deltas_for_tests, read_snapshot,
};
use rdy_store::sync_stream::StreamRecord;
use rdy_store::{Store, StoreError, kinds};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

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

// ---------------------------------------------------------------------------------------------
// sdl2: the one map read

/// A block id from `n`: prefix `b` and five Crockford digits.
fn nid(n: usize) -> BlockId {
    let digits: String = (0..5)
        .rev()
        .map(|i| rdy_model::CROCKFORD[(n >> (5 * i)) & 31] as char)
        .collect();
    id(&format!("b-{digits}"))
}

/// A landing record on `prev` with state `state`; `n` keeps landings of one state distinct.
fn landing_rec(prev: Option<Hash>, state: Hash, n: i64) -> LandingRecord {
    LandingRecord {
        proposal: Hash([0xAB; 32]),
        prev,
        state,
        approved_by: Provenance {
            actor: "human:a".into(),
            run: None,
            role: Role::Approver,
            at_ms: n,
        },
        changes: vec![],
        chosen: None,
        checks: vec![],
        assurance: Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        approved_version: None,
        rebased_from: vec![],
        evidence: None,
        assessment: None,
        policy: None,
        read_identity: None,
    }
}

/// One landing of a written history: its hash, its record, its delta and the delta's hash.
#[derive(Clone)]
struct Landed {
    hash: Hash,
    rec: LandingRecord,
    delta: SnapshotDelta,
    delta_hash: Hash,
}

/// Writes a chain with one landing per map of `maps`, each with its delta. The landing at an
/// index in `held` also gets its SNAPSHOT; no other SNAPSHOT is written.
fn history(s: &mut Store, maps: &[BTreeMap<BlockId, Hash>], held: &[usize]) -> Vec<Landed> {
    let mut out: Vec<Landed> = vec![];
    let empty = BTreeMap::new();
    for (i, m) in maps.iter().enumerate() {
        let prev = out.last().cloned();
        let rec = landing_rec(prev.as_ref().map(|p| p.hash), snapshot_map_id(m), i as i64);
        if held.contains(&i) {
            let snap = StateSnapshot { blocks: m.clone() };
            assert_eq!(s.put(kinds::SNAPSHOT, &snap).unwrap(), rec.state);
        }
        let hash = s.put(kinds::LANDING, &rec).unwrap();
        let before = if i == 0 { &empty } else { &maps[i - 1] };
        let delta = diff_snapshots(hash, prev.as_ref().map(|p| p.delta_hash), before, m);
        let delta_hash = s.put(kinds::SNAPSHOT_DELTA, &delta).unwrap();
        assert_eq!(delta_hash, delta.id());
        out.push(Landed {
            hash,
            rec,
            delta,
            delta_hash,
        });
    }
    out
}

/// Six maps: additions, a change, a drop, a no-op landing and a re-add.
fn six_maps() -> Vec<BTreeMap<BlockId, Hash>> {
    let m0 = map(&[("A-00001", 1), ("B-00002", 2)]);
    let mut m1 = m0.clone();
    m1.insert(id("C-00003"), h(3));
    let mut m2 = m1.clone();
    m2.insert(id("A-00001"), h(4));
    let mut m3 = m2.clone();
    m3.remove(&id("B-00002"));
    let m4 = m3.clone(); // a no-op landing: its own delta, the same full hash
    let mut m5 = m4.clone();
    m5.insert(id("B-00002"), h(2));
    m5.insert(id("D-00004"), h(5));
    vec![m0, m1, m2, m3, m4, m5]
}

/// The deltas folded by `f`, and its result.
fn folds<T>(f: impl FnOnce() -> T) -> (u64, T) {
    let start = folded_deltas_for_tests();
    let out = f();
    (folded_deltas_for_tests() - start, out)
}

fn store() -> (tempfile::TempDir, std::path::PathBuf, Store) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("s");
    let s = Store::create(&path).unwrap();
    (d, path, s)
}

#[test]
fn sdl2_snapshot_read_prefers_held_map_then_folds() {
    let _staged = stage_snapshot_delta_for_tests();
    let (_d, _p, mut s) = store();
    let maps = six_maps();
    // only landing 2 holds its SNAPSHOT
    let ls = history(&mut s, &maps, &[2]);
    assert_eq!(
        ls[3].rec.state, ls[4].rec.state,
        "a no-op keeps its full hash"
    );
    assert!(ls[4].delta.set.is_empty() && ls[4].delta.drop.is_empty());
    let r = s.reader();
    // the held map answers with no fold
    r.clear_map_cache_for_tests();
    let (n, got) = folds(|| r.snapshot_at(&ls[2].hash).unwrap());
    assert_eq!((n, got.blocks), (0, maps[2].clone()));
    // a later landing folds from the nearest held map: landings 3, 4 and 5
    r.clear_map_cache_for_tests();
    let (n, got) = folds(|| r.snapshot_at(&ls[5].hash).unwrap());
    assert_eq!((n, got.blocks), (3, maps[5].clone()));
    // an earlier one folds from the empty map before the first landing: landings 0 and 1
    r.clear_map_cache_for_tests();
    let (n, got) = folds(|| r.snapshot_at(&ls[1].hash).unwrap());
    assert_eq!((n, got.blocks), (2, maps[1].clone()));
    // the first landing's delta holds its whole map
    assert_eq!(ls[0].delta.set.len(), maps[0].len());
    assert_eq!(ls[0].delta.base, None);
    // every landing reads its own map; state_of and the cached reads agree
    r.clear_map_cache_for_tests();
    for (l, m) in ls.iter().zip(&maps) {
        assert_eq!(r.snapshot_at(&l.hash).unwrap().blocks, *m);
        assert_eq!(r.state_of(Some(l.hash)).unwrap().blocks, *m);
    }
    assert_eq!(r.state_of(None).unwrap(), StateSnapshot::default());
    // while held SNAPSHOTs are set aside, landing 2 folds too, from the first landing
    r.fold_held_snapshots_for_tests(true);
    let (n, got) = folds(|| r.snapshot_at(&ls[2].hash).unwrap());
    assert_eq!((n, got.blocks), (3, maps[2].clone()));
    r.fold_held_snapshots_for_tests(false);
    // a landing with no delta reads its held SNAPSHOT as before deltas
    let legacy = landing_rec(Some(ls[5].hash), snapshot_map_id(&maps[0]), 100);
    let snap = StateSnapshot {
        blocks: maps[0].clone(),
    };
    s.put(kinds::SNAPSHOT, &snap).unwrap();
    let lh = s.put(kinds::LANDING, &legacy).unwrap();
    let r = s.reader();
    r.fold_held_snapshots_for_tests(true);
    let (n, got) = folds(|| r.snapshot_at(&lh).unwrap());
    assert_eq!((n, got), (0, snap));
}

#[test]
fn sdl2_cold_warm_and_evicted_reads_are_equal() {
    let _staged = stage_snapshot_delta_for_tests();
    let (_d, _p, mut s) = store();
    let maps = six_maps();
    let ls = history(&mut s, &maps, &[]);
    let r = s.reader();
    r.clear_map_cache_for_tests();
    let (cold_n, cold) = folds(|| r.snapshot_at(&ls[5].hash).unwrap());
    assert_eq!(
        cold_n, 6,
        "a cold read folds every delta from the first landing"
    );
    assert_eq!(cold.blocks, maps[5]);
    let (warm_n, warm) = folds(|| r.snapshot_at(&ls[5].hash).unwrap());
    assert_eq!((warm_n, &warm), (0, &cold), "a warm read folds nothing");
    // a clone of the reader shares the cache
    let (n, _) = folds(|| r.clone().snapshot_at(&ls[5].hash).unwrap());
    assert_eq!(n, 0);
    // a cached earlier map is where a later fold starts
    r.clear_map_cache_for_tests();
    let (n, got) = folds(|| r.snapshot_at(&ls[2].hash).unwrap());
    assert_eq!((n, got.blocks), (3, maps[2].clone()));
    let (n, got) = folds(|| r.snapshot_at(&ls[5].hash).unwrap());
    assert_eq!(
        (n, got),
        (3, cold.clone()),
        "folds 3, 4 and 5 from the cached 2"
    );
    // the no-op landing 4 shares landing 3's full hash, so 3's cached map answers it
    let (n, _) = folds(|| r.snapshot_at(&ls[3].hash).unwrap());
    assert_eq!(n, 1, "landing 3 folds from the cached 2");
    let (n, got) = folds(|| r.snapshot_at(&ls[4].hash).unwrap());
    assert_eq!((n, got.blocks), (0, maps[4].clone()));
    // evicted: with no budget nothing is kept, and every read is cold and equal
    r.set_map_cache_budget_for_tests(0);
    assert_eq!(r.map_cache_stats_for_tests(), (0, 0));
    for _ in 0..2 {
        let (n, got) = folds(|| r.snapshot_at(&ls[5].hash).unwrap());
        assert_eq!((n, &got), (6, &cold));
    }
}

#[test]
fn sdl2_cache_cap_and_source_epochs_do_not_change_reads() {
    let _staged = stage_snapshot_delta_for_tests();
    let (_d, path, mut s) = store();
    let maps = six_maps();
    let ls = history(&mut s, &maps, &[]);
    let want: Vec<StateSnapshot> = maps
        .iter()
        .map(|m| StateSnapshot { blocks: m.clone() })
        .collect();
    // the cap: room for about one map, so the oldest entry goes first
    let r = s.reader();
    r.clear_map_cache_for_tests();
    r.snapshot_at(&ls[1].hash).unwrap();
    let (one, bytes) = r.map_cache_stats_for_tests();
    assert_eq!(one, 1);
    r.set_map_cache_budget_for_tests(bytes + bytes / 2);
    assert_eq!(r.snapshot_at(&ls[2].hash).unwrap(), want[2]);
    assert_eq!(
        r.map_cache_stats_for_tests().0,
        1,
        "the oldest entry was evicted"
    );
    let (n, got) = folds(|| r.snapshot_at(&ls[1].hash).unwrap());
    assert_eq!(
        (n, got),
        (2, want[1].clone()),
        "landing 1 was the one evicted"
    );
    // an entry larger than the budget is not kept; its read still answers
    r.set_map_cache_budget_for_tests(1);
    assert_eq!(r.map_cache_stats_for_tests(), (0, 0));
    assert_eq!(r.snapshot_at(&ls[5].hash).unwrap(), want[5]);
    assert_eq!(r.map_cache_stats_for_tests(), (0, 0));
    r.set_map_cache_budget_for_tests(rdy_store::snapshot::MAP_CACHE_BYTES);
    // a reindex empties the cache; the reads are the same
    r.snapshot_at(&ls[5].hash).unwrap();
    assert_eq!(s.map_cache_stats_for_tests().0, 1);
    s.reindex().unwrap();
    assert_eq!(s.map_cache_stats_for_tests(), (0, 0));
    for (l, w) in ls.iter().zip(&want) {
        assert_eq!(s.snapshot_at(&l.hash).unwrap(), *w);
    }
    // a reader taken before the reindex reads through its own index, and its maps are not
    // served to the store's reader
    let old = s.reader().clone();
    s.reindex().unwrap();
    assert_eq!(old.snapshot_at(&ls[3].hash).unwrap(), want[3]);
    let (n, _) = folds(|| s.snapshot_at(&ls[3].hash).unwrap());
    assert_eq!(n, 4, "an entry of another index is never served");
    drop(old);
    // a reopen starts with an empty cache; the reads are the same
    s.close().unwrap();
    let s = Store::open(&path).unwrap();
    assert_eq!(s.map_cache_stats_for_tests(), (0, 0));
    let (n, got) = folds(|| s.snapshot_at(&ls[5].hash).unwrap());
    assert_eq!((n, got), (6, want[5].clone()));
    // another store with the same landings but no deltas: the first store's maps are never
    // its answer
    let (_d2, _p2, mut other) = store();
    for l in &ls {
        other.put(kinds::LANDING, &l.rec).unwrap();
    }
    assert!(matches!(
        other.snapshot_at(&ls[5].hash),
        Err(StoreError::NotFound(h)) if h == ls[5].rec.state
    ));
    // the cache's own tags: another source or an older epoch is never served or kept
    let cache = MapCache::new();
    let a = CacheTag {
        source: 1,
        epoch: 1,
    };
    let m = Arc::new(maps[0].clone());
    cache.insert(a, ls[0].rec.state, Arc::clone(&m));
    assert_eq!(cache.get(a, &ls[0].rec.state), Some(Arc::clone(&m)));
    let other_source = CacheTag {
        source: 2,
        epoch: 1,
    };
    assert_eq!(cache.get(other_source, &ls[0].rec.state), None);
    let older = CacheTag {
        source: 1,
        epoch: 0,
    };
    cache.insert(older, ls[1].rec.state, Arc::new(maps[1].clone()));
    assert_eq!(cache.stats().0, 1, "an older epoch keeps nothing");
    assert_eq!(cache.get(older, &ls[0].rec.state), None);
    let newer = CacheTag {
        source: 1,
        epoch: 2,
    };
    cache.insert(newer, ls[1].rec.state, Arc::new(maps[1].clone()));
    assert_eq!(
        cache.get(a, &ls[0].rec.state),
        None,
        "a newer epoch drops the older"
    );
    assert_eq!(cache.stats().0, 1);
}

/// The record of `value` of `kind` under the key `hash`, which need not be its hash.
fn record_of<T: serde::Serialize + 'static>(
    kind: u8,
    hash: Hash,
    value: &T,
) -> (ObjectKey, StreamRecord) {
    let o = codec::encode_new(kind, value);
    let key = ObjectKey { kind, hash };
    (
        key,
        StreamRecord::Present {
            key,
            encoding: o.version,
            payload: o.payload,
        },
    )
}

#[test]
fn sdl2_fold_refuses_cycles_missing_bases_and_wrong_kinds() {
    let _staged = stage_snapshot_delta_for_tests();
    let maps = six_maps();
    let decode = |r: Result<StateSnapshot, StoreError>| match r {
        Err(StoreError::Decode(m)) => m,
        other => panic!("not a decode refusal: {other:?}"),
    };

    // a missing base: landing 1 has no delta, so landing 2's fold cannot reach a map
    let (_d, _p, mut s) = store();
    let mut ls = vec![];
    let empty = BTreeMap::new();
    for (i, m) in maps.iter().take(3).enumerate() {
        let prev: Option<&Landed> = ls.last();
        let rec = landing_rec(prev.map(|p| p.hash), snapshot_map_id(m), i as i64);
        let hash = s.put(kinds::LANDING, &rec).unwrap();
        let before = if i == 0 { &empty } else { &maps[i - 1] };
        let delta = diff_snapshots(hash, prev.map(|p| p.delta_hash), before, m);
        if i != 1 {
            s.put(kinds::SNAPSHOT_DELTA, &delta).unwrap();
        }
        ls.push(Landed {
            hash,
            rec,
            delta_hash: delta.id(),
            delta,
        });
    }
    assert_eq!(
        decode(s.snapshot_at(&ls[2].hash)),
        format!(
            "landing {}: expected exactly one snapshot delta",
            ls[1].hash
        )
    );
    // with neither its SNAPSHOT nor a delta, a landing of a store without the feature reads as
    // the missing object it always was
    let (_d0, _p0, mut legacy) = store();
    let lone = legacy.put(kinds::LANDING, &ls[0].rec).unwrap();
    assert!(matches!(
        legacy.snapshot_at(&lone),
        Err(StoreError::NotFound(h)) if h == ls[0].rec.state
    ));
    // in a store that lists the feature, it needs its delta
    assert_eq!(
        decode(s.snapshot_at(&ls[1].hash)),
        format!(
            "landing {}: expected exactly one snapshot delta",
            ls[1].hash
        )
    );

    // the chain checks, each on a fresh two-landing history whose second delta is bent
    let bent = |edit: &dyn Fn(&mut SnapshotDelta, &Landed)| {
        let (d, p, mut s) = store();
        let ls = history(&mut s, &maps[..1], &[]);
        let rec = landing_rec(Some(ls[0].hash), snapshot_map_id(&maps[1]), 1);
        let hash = s.put(kinds::LANDING, &rec).unwrap();
        let mut delta = diff_snapshots(hash, Some(ls[0].delta_hash), &maps[0], &maps[1]);
        edit(&mut delta, &ls[0]);
        let dh = s.put(kinds::SNAPSHOT_DELTA, &delta).unwrap();
        (d, p, s, hash, dh)
    };
    let (_d, _p, s, l, dh) = bent(&|d, _| d.base = Some(h(77)));
    assert_eq!(
        decode(s.snapshot_at(&l)),
        format!("snapshot delta {dh}: base does not match the previous landing")
    );
    let (_d, _p, s, l, dh) = bent(&|d, _| d.base = None);
    assert_eq!(
        decode(s.snapshot_at(&l)),
        format!("snapshot delta {dh}: base does not match the previous landing")
    );
    let (_d, _p, s, l, dh) = bent(&|d, _| d.full = h(78));
    assert_eq!(
        decode(s.snapshot_at(&l)),
        format!("snapshot delta {dh}: landing or full hash does not match")
    );
    let (_d, _p, s, l, dh) = bent(&|d, _| d.drop.push(id("Z-00099")));
    assert_eq!(
        decode(s.snapshot_at(&l)),
        format!("snapshot delta {dh}: set or drop is not canonical")
    );
    let (_d, _p, s, l, dh) = bent(&|d, _| d.set.push((id("Z-00099"), h(9))));
    assert_eq!(
        decode(s.snapshot_at(&l)),
        format!("snapshot delta {dh}: folded map does not match full")
    );

    // wrong kinds: a state that names an object of another kind is no map
    let (_d, _p, mut s) = store();
    let ls = history(&mut s, &maps[..1], &[]);
    let rec = landing_rec(None, ls[0].delta_hash, 5);
    let wrong = s.put(kinds::LANDING, &rec).unwrap();
    assert_eq!(
        decode(s.snapshot_at(&wrong)),
        format!(
            "{} is not a snapshot (object kind {})",
            ls[0].delta_hash,
            30 | 0x80
        )
    );
    // a landing hash that names another kind is not a landing
    assert!(s.snapshot_at(&ls[0].delta_hash).is_err());

    // growth: a delta that would take the map past 100000 entries is refused before it grows
    let (_d, _p, mut s) = store();
    let big0: BTreeMap<BlockId, Hash> = (0..60_000).map(|n| (nid(n), h(1))).collect();
    let big1: BTreeMap<BlockId, Hash> = (0..100_001).map(|n| (nid(n), h(1))).collect();
    let ls = history(&mut s, &[big0], &[]);
    let rec = landing_rec(Some(ls[0].hash), snapshot_map_id(&big1), 1);
    let l = s.put(kinds::LANDING, &rec).unwrap();
    let delta = diff_snapshots(l, Some(ls[0].delta_hash), &ls_map(&ls[0]), &big1);
    let dh = s.put(kinds::SNAPSHOT_DELTA, &delta).unwrap();
    assert_eq!(
        decode(s.snapshot_at(&l)),
        format!("snapshot delta {dh}: exceeds 100000 collection elements")
    );

    // a cycle: stream records whose landing names itself as its `prev` (the key is not the
    // record's hash, so no store could hold it); the walk refuses it without recursing
    let m = maps[0].clone();
    let x = h(0x51);
    let mut rec = landing_rec(Some(x), snapshot_map_id(&m), 1);
    rec.prev = Some(x);
    let delta = SnapshotDelta {
        landing: x,
        base: Some(h(0x52)),
        full: rec.state,
        set: vec![],
        drop: vec![],
    };
    let records: BTreeMap<ObjectKey, StreamRecord> = [
        record_of(kinds::LANDING, x, &rec),
        record_of(kinds::SNAPSHOT_DELTA, h(0x52), &delta),
    ]
    .into_iter()
    .collect();
    let mut src = records;
    let e = read_snapshot(&mut RecordSnapshots::new(&mut src), None, &x).unwrap_err();
    assert_eq!(e.code, "E_BAD_RECORD");
    assert_eq!(e.message, format!("dependency cycle through {x}"));
}

/// The full map a landing's single delta sets (a first landing's).
fn ls_map(l: &Landed) -> BTreeMap<BlockId, Hash> {
    l.delta.set.iter().cloned().collect()
}
