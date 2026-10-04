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
                IndexEntry::Object(h, loc),
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
    }
    let idx = Index::open(&path).unwrap();
    assert_eq!(idx.object(&h).unwrap(), Some(loc));
    assert_eq!(idx.producer(&v).unwrap(), Some(c));
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
