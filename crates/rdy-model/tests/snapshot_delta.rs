//! sdl1 (snapshot deltas, D-RP6SM): a landing's snapshot recorded as the difference from the
//! previous landing's map. The record's field order and encoding are fixed, `StateSnapshot`'s
//! identity and bytes never change, the pure diff and fold agree, and the canonical-form check
//! refuses every redundant delta.
use rdy_model::encoding::{SNAPSHOT_DELTA_ENCODING, encoded_hash};
use rdy_model::{
    BlockId, Hash, SnapshotDelta, SnapshotDeltaError, StateSnapshot, diff_snapshots,
    fold_snapshot_delta, snapshot_map_id, validate_snapshot_delta_form,
};
use std::collections::BTreeMap;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

fn map(pairs: &[(&str, u8)]) -> BTreeMap<BlockId, Hash> {
    pairs.iter().map(|(i, n)| (id(i), h(*n))).collect()
}

/// The postcard bytes of a short string: its one-byte length, then its bytes.
fn text(s: &str) -> Vec<u8> {
    assert!(s.len() < 128);
    let mut out = vec![s.len() as u8];
    out.extend_from_slice(s.as_bytes());
    out
}

#[test]
fn sdl1_field_order_and_snapshot_identity_are_fixed() {
    // the record is {landing, base, full, set, drop}, in that order, at encoding 1
    assert_eq!(SNAPSHOT_DELTA_ENCODING, 1);
    let d = SnapshotDelta {
        landing: h(1),
        base: Some(h(2)),
        full: h(3),
        set: vec![(id("A-00001"), h(4))],
        drop: vec![id("B-00002")],
    };
    let mut want = vec![1u8; 32];
    want.push(1);
    want.extend([2u8; 32]);
    want.extend([3u8; 32]);
    want.push(1);
    want.extend(text("A-00001"));
    want.extend([4u8; 32]);
    want.push(1);
    want.extend(text("B-00002"));
    let body = postcard::to_allocvec(&d).unwrap();
    assert_eq!(body, want, "field order and encoding");
    // the id is the hash of the stored bytes: the two-byte encoding prefix, then the body
    let mut stored = vec![1u8, 0];
    stored.extend(&want);
    assert_eq!(d.id(), Hash::of(&stored));
    assert_eq!(d.id(), encoded_hash(SNAPSHOT_DELTA_ENCODING, &body));
    assert_ne!(d.id(), Hash::of(&body), "the prefix is hashed");
    // an absent base is the option's zero tag
    let first = SnapshotDelta {
        base: None,
        ..d.clone()
    };
    let body = postcard::to_allocvec(&first).unwrap();
    assert_eq!(body[32], 0);
    assert_eq!(body.len(), want.len() - 32);
    let back: SnapshotDelta = postcard::from_bytes(&body).unwrap();
    assert_eq!(back, first);

    // StateSnapshot keeps its encoding-0 bytes and its id: a postcard map, hashed bare
    let empty = StateSnapshot::default();
    assert_eq!(empty.canonical_bytes(), vec![0u8]);
    assert_eq!(empty.id(), Hash::of(&[0u8]));
    let one = StateSnapshot {
        blocks: map(&[("A-00001", 4)]),
    };
    let mut bytes = vec![1u8];
    bytes.extend(text("A-00001"));
    bytes.extend([4u8; 32]);
    assert_eq!(one.canonical_bytes(), bytes);
    assert_eq!(postcard::to_allocvec(&one).unwrap(), bytes);
    assert_eq!(one.id(), Hash::of(&bytes));
    assert!(one.is_encoding_of(&bytes));
    // the map's id needs no StateSnapshot built around it
    assert_eq!(snapshot_map_id(&one.blocks), one.id());
    assert_eq!(snapshot_map_id(&BTreeMap::new()), empty.id());
    // `full` is that id: a delta of the first landing names it
    let d = diff_snapshots(h(9), None, &BTreeMap::new(), &one.blocks);
    assert_eq!(d.full, one.id());
}

#[test]
fn sdl1_diff_fold_covers_add_change_drop_and_noop() {
    let empty = BTreeMap::new();
    // the first delta sets the complete first map, sorted by the ids' Rust order ('B' < 'a')
    let first = map(&[("a-00001", 1), ("B-00001", 2), ("C-00003", 3)]);
    let d1 = diff_snapshots(h(10), None, &empty, &first);
    assert_eq!(d1.landing, h(10));
    assert_eq!(d1.base, None);
    assert_eq!(
        d1.set,
        vec![
            (id("B-00001"), h(2)),
            (id("C-00003"), h(3)),
            (id("a-00001"), h(1)),
        ]
    );
    assert!(d1.drop.is_empty());
    assert_eq!(d1.full, snapshot_map_id(&first));
    // reverse insertion order gives the same map and the same delta bytes
    let mut reversed = BTreeMap::new();
    for (k, v) in first.iter().rev() {
        reversed.insert(k.clone(), *v);
    }
    let r1 = diff_snapshots(h(10), None, &empty, &reversed);
    assert_eq!(postcard::to_allocvec(&r1), postcard::to_allocvec(&d1));
    assert_eq!(r1.id(), d1.id());

    // a later delta: one add, one change, one drop; the kept pair is in neither list
    let next = map(&[("a-00001", 1), ("B-00001", 7), ("D-00004", 4)]);
    let d2 = diff_snapshots(h(11), Some(d1.id()), &first, &next);
    assert_eq!(d2.base, Some(d1.id()));
    assert_eq!(d2.set, vec![(id("B-00001"), h(7)), (id("D-00004"), h(4))]);
    assert_eq!(d2.drop, vec![id("C-00003")]);
    validate_snapshot_delta_form(&first, &d2).unwrap();
    let mut folded = first.clone();
    fold_snapshot_delta(&mut folded, &d2).unwrap();
    assert_eq!(folded, next);
    assert_eq!(snapshot_map_id(&folded), d2.full);

    // a no-op landing still has its own delta: nothing set, nothing dropped
    let d3 = diff_snapshots(h(12), Some(d2.id()), &next, &next);
    assert!(d3.set.is_empty() && d3.drop.is_empty());
    assert_eq!(d3.full, d2.full);
    let mut same = next.clone();
    fold_snapshot_delta(&mut same, &d3).unwrap();
    assert_eq!(same, next);
    // identical full maps have different landing-specific deltas
    let d4 = diff_snapshots(h(13), Some(d3.id()), &next, &next);
    assert_eq!(d4.full, d3.full);
    assert_ne!(d4.id(), d3.id());

    // dropping everything, and an empty first map
    let d5 = diff_snapshots(h(14), Some(d4.id()), &next, &empty);
    assert!(d5.set.is_empty());
    assert_eq!(d5.drop.len(), 3);
    let mut gone = next.clone();
    fold_snapshot_delta(&mut gone, &d5).unwrap();
    assert!(gone.is_empty());
    assert_eq!(d5.full, StateSnapshot::default().id());
    let d0 = diff_snapshots(h(15), None, &empty, &empty);
    assert!(d0.set.is_empty() && d0.drop.is_empty());
    assert_eq!(d0.full, StateSnapshot::default().id());

    // the set's length varint at its one-byte boundary: 127 entries take one byte, 128 take two
    for (n, len) in [(127usize, vec![0x7fu8]), (128, vec![0x80, 0x01])] {
        let big: BTreeMap<BlockId, Hash> = (0..n)
            .map(|i| (id(&format!("A-{:05}", i)), h((i % 250) as u8)))
            .collect();
        let d = diff_snapshots(h(16), None, &empty, &big);
        let body = postcard::to_allocvec(&d).unwrap();
        // landing (32), base tag (1), full (32), then the set's length
        assert_eq!(&body[65..65 + len.len()], &len[..], "{n} entries");
        let mut built = BTreeMap::new();
        fold_snapshot_delta(&mut built, &d).unwrap();
        assert_eq!(built, big);
        assert_eq!(snapshot_map_id(&built), d.full);
        let snap = StateSnapshot { blocks: big };
        assert_eq!(&snap.canonical_bytes()[..len.len()], &len[..]);
    }
}

#[test]
fn sdl1_canonical_form_rejects_every_redundancy() {
    let prev = map(&[("A-00001", 1), ("B-00002", 2), ("C-00003", 3)]);
    let good = SnapshotDelta {
        landing: h(20),
        base: Some(h(21)),
        full: h(22),
        set: vec![(id("B-00002"), h(9)), (id("D-00004"), h(4))],
        drop: vec![id("A-00001"), id("C-00003")],
    };
    validate_snapshot_delta_form(&prev, &good).unwrap();
    let with = |set: Vec<(&str, u8)>, drop: Vec<&str>| SnapshotDelta {
        set: set.into_iter().map(|(i, n)| (id(i), h(n))).collect(),
        drop: drop.into_iter().map(id).collect(),
        ..good.clone()
    };
    let cases: Vec<(SnapshotDelta, SnapshotDeltaError)> = vec![
        (
            with(vec![("D-00004", 4), ("D-00004", 5)], vec![]),
            SnapshotDeltaError::SetRepeat(id("D-00004")),
        ),
        (
            with(vec![("E-00005", 5), ("D-00004", 4)], vec![]),
            SnapshotDeltaError::SetUnsorted(id("D-00004")),
        ),
        (
            // the ids' Rust order, never a locale's: 'a' sorts after 'D'
            with(vec![("a-00004", 4), ("D-00004", 4)], vec![]),
            SnapshotDeltaError::SetUnsorted(id("D-00004")),
        ),
        (
            with(vec![], vec!["A-00001", "A-00001"]),
            SnapshotDeltaError::DropRepeat(id("A-00001")),
        ),
        (
            with(vec![], vec!["C-00003", "A-00001"]),
            SnapshotDeltaError::DropUnsorted(id("A-00001")),
        ),
        (
            with(vec![("B-00002", 9)], vec!["B-00002"]),
            SnapshotDeltaError::Overlap(id("B-00002")),
        ),
        (
            with(vec![("B-00002", 2)], vec![]),
            SnapshotDeltaError::Unchanged(id("B-00002")),
        ),
        (
            with(vec![], vec!["D-00004"]),
            SnapshotDeltaError::AbsentDrop(id("D-00004")),
        ),
    ];
    for (d, want) in cases {
        assert_eq!(
            validate_snapshot_delta_form(&prev, &d),
            Err(want.clone()),
            "{d:?}"
        );
        // the fold checks first and leaves the map as it was
        let mut m = prev.clone();
        assert_eq!(fold_snapshot_delta(&mut m, &d), Err(want));
        assert_eq!(m, prev);
    }
    // a first delta's set is checked against the empty map: nothing to drop
    let first = SnapshotDelta {
        base: None,
        ..with(vec![("A-00001", 1)], vec!["A-00001"])
    };
    assert_eq!(
        validate_snapshot_delta_form(&BTreeMap::new(), &first),
        Err(SnapshotDeltaError::Overlap(id("A-00001")))
    );
    // every refusal reads as one non-canonical delta
    assert_eq!(
        SnapshotDeltaError::Unchanged(id("B-00002")).to_string(),
        "set or drop is not canonical: block B-00002 is set to the version it already has"
    );
}
