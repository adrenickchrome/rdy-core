use rdy_model::Hash;
use rdy_store::index::{Index, IndexEntry};
use rdy_store::segment::Location;

#[test]
fn fresh_index_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let idx = Index::open(&dir.path().join("index.redb")).unwrap();
    assert_eq!(
        idx.indexed_upto().unwrap(),
        Location {
            segment: 0,
            offset: 0
        }
    );
    assert_eq!(idx.object(&Hash::of(b"x")).unwrap(), None);
    assert_eq!(idx.get_ref("canon").unwrap(), None);
}

#[test]
fn apply_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    let (h, v, c) = (Hash::of(b"obj"), Hash::of(b"ver"), Hash::of(b"chg"));
    let loc = Location {
        segment: 2,
        offset: 99,
    };
    {
        let idx = Index::open(&path).unwrap();
        idx.apply(
            &[
                IndexEntry::Object {
                    kind: rdy_store::kinds::BLOB_CHUNK,
                    hash: h,
                    loc,
                },
                IndexEntry::Producer {
                    version: v,
                    change: c,
                },
                IndexEntry::Ref("view/work".into(), vec![1, 2]),
                IndexEntry::Ref("view/other".into(), vec![3]),
                IndexEntry::Ref("canon".into(), vec![9]),
            ],
            Location {
                segment: 2,
                offset: 200,
            },
        )
        .unwrap();
        idx.checkpoint().unwrap(); // `apply` alone is not durable
    }
    let idx = Index::open(&path).unwrap();
    assert_eq!(idx.object(&h).unwrap(), Some(loc));
    // merge1 fix round 1: a version keeps every producer, so the index lists them
    assert_eq!(idx.producers(&v).unwrap(), vec![c]);
    assert_eq!(idx.get_ref("canon").unwrap(), Some(vec![9]));
    let views = idx.refs_with_prefix("view/").unwrap();
    assert_eq!(
        views,
        vec![
            ("view/other".to_string(), vec![3]),
            ("view/work".to_string(), vec![1, 2])
        ]
    );
    assert_eq!(
        idx.indexed_upto().unwrap(),
        Location {
            segment: 2,
            offset: 200
        }
    );
}

#[test]
fn a_malformed_producer_entry_is_a_decode_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    drop(Index::open(&path).unwrap());
    {
        let db = redb::Database::create(&path).unwrap();
        let w = db.begin_write().unwrap();
        {
            let mut t = w
                .open_table(redb::TableDefinition::<&[u8], &[u8]>::new("producers"))
                .unwrap();
            // merge1 fix round 1: an entry is the version then the change, so a key with a short
            // change hash is the malformed entry
            let mut key = vec![7u8; 32];
            key.extend_from_slice(b"short");
            t.insert(&key[..], &[][..]).unwrap();
        }
        w.commit().unwrap();
    }
    let idx = Index::open(&path).unwrap();
    assert!(matches!(
        idx.producers(&Hash([7u8; 32])),
        Err(rdy_store::StoreError::Decode(_))
    ));
}

/// Final review (Important 2), pinning what redb 4.3 reports, which `open_index`'s classification
/// rests on: a copy of a live index (a non-durable commit pending, as a kill leaves it) is
/// repaired at open (the repair callback fires) and `check_integrity` then reports it clean. So a
/// kill reaches the trusted `Ok` arm through `clean`, not through the sentinel; the sentinel only
/// decides the `Ok(clean == false)` arm, which redb 4.3 was not seen to produce. If a redb upgrade
/// changes this, revisit `open_index`.
#[test]
fn kill1_redb_repairs_a_copy_of_a_live_index_at_open_and_then_reports_it_clean() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    let idx = Index::open(&path).unwrap();
    let upto = Location {
        segment: 0,
        offset: 64,
    };
    idx.apply(
        &[IndexEntry::Object {
            kind: rdy_store::kinds::BLOB_CHUNK,
            hash: Hash::of(b"pending"),
            loc: upto,
        }],
        upto,
    )
    .unwrap();
    let copy = dir.path().join("copy.redb");
    std::fs::copy(&path, &copy).unwrap();
    let c = Index::open_checked(&copy).unwrap();
    assert!(c.repaired_at_open, "redb repairs a killed file at open");
    assert!(c.clean, "and check_integrity reports it clean afterwards");
    drop(c);
    let c = Index::open_checked(&copy).unwrap();
    assert!(c.clean && !c.repaired_at_open, "a second open is clean");
    drop(c);
    drop(idx);
    let c = Index::open_checked(&path).unwrap();
    assert!(c.clean && !c.repaired_at_open, "a cleanly closed file");
}

#[test]
fn kind1_typed_entries_coexist_and_the_untyped_entry_is_the_first_written() {
    let dir = tempfile::tempdir().unwrap();
    let idx = Index::open(&dir.path().join("index.redb")).unwrap();
    let h = Hash::of(b"same bytes");
    let (a, b) = (
        Location {
            segment: 0,
            offset: 10,
        },
        Location {
            segment: 0,
            offset: 90,
        },
    );
    idx.apply(
        &[
            IndexEntry::Object {
                kind: rdy_store::kinds::BLOB_CHUNK,
                hash: h,
                loc: a,
            },
            IndexEntry::Object {
                kind: rdy_store::kinds::SNAPSHOT,
                hash: h,
                loc: b,
            },
        ],
        Location {
            segment: 0,
            offset: 200,
        },
    )
    .unwrap();
    assert_eq!(
        idx.typed(rdy_store::kinds::BLOB_CHUNK, &h).unwrap(),
        Some(a)
    );
    assert_eq!(idx.typed(rdy_store::kinds::SNAPSHOT, &h).unwrap(), Some(b));
    assert_eq!(idx.typed(rdy_store::kinds::TAG, &h).unwrap(), None);
    assert_eq!(
        idx.object(&h).unwrap(),
        Some(a),
        "untyped: the first written"
    );
}
