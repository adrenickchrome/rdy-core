//! The project's metadata refs (store format 12, plan Export and clone): `meta/repo_id` in a new
//! store's first batch, `meta/origin`, the `set_refs` guard and the fsck rules.
use rdy_model::Hash;
use rdy_store::dump::{DumpBatch, DumpHeader, DumpWriter, Frame, READS, frame_kinds};
use rdy_store::format::{REF_STORE_FORMAT, STORE_FORMAT, encode_u32};
use rdy_store::kinds::{self, RefUpdate, batch_digest};
use rdy_store::meta::{
    FIRST_REPO_ID_STORE_FORMAT, Origin, REF_ORIGIN, REF_REPO_ID, decode_origin, decode_repo_id,
    encode_origin, encode_repo_id,
};
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError};

fn new_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::create(&dir.path().join("s")).unwrap();
    (dir, s)
}

fn fsck_text(s: &Store) -> String {
    match s.check_objects() {
        Err(StoreError::Decode(m)) => m,
        other => panic!("{other:?}"),
    }
}

#[test]
fn ec1_a_new_store_writes_its_repo_id_in_its_first_batch() {
    assert_eq!(FIRST_REPO_ID_STORE_FORMAT, 12);
    let (_d, s) = new_store();
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            batches.push(b);
            Ok(())
        })
        .unwrap();
    assert_eq!(batches.len(), 1, "one format batch");
    let first = &batches[0];
    assert_eq!(first.start, origin);
    let refs: Vec<RefUpdate> = first
        .frames
        .iter()
        .map(|f| {
            assert_eq!(f.kind, kinds::REF_UPDATE);
            postcard::from_bytes(&f.payload).unwrap()
        })
        .collect();
    let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, [REF_STORE_FORMAT, REF_REPO_ID]);
    assert_eq!(refs[0].value, encode_u32(STORE_FORMAT));
    let id = s.reader().repo_id().unwrap().expect("a repo id");
    assert_eq!(refs[1].value, encode_repo_id(&id));
    assert_eq!(refs[1].value.len(), 16);
    assert_eq!(decode_repo_id(&refs[1].value), Some(id));
    assert_eq!(
        s.reader().origin().unwrap(),
        None,
        "a new store has no origin"
    );
    s.check_objects().unwrap();
}

#[test]
fn ec1_two_new_stores_get_different_repo_ids() {
    let (_a, a) = new_store();
    let (_b, b) = new_store();
    let a = a.reader().repo_id().unwrap().unwrap();
    let b = b.reader().repo_id().unwrap().unwrap();
    assert_ne!(a, b);
}

#[test]
fn ec1_set_refs_refuses_every_meta_ref() {
    let (_d, mut s) = new_store();
    for name in [REF_REPO_ID, REF_ORIGIN, REF_STORE_FORMAT, "meta/x"] {
        let end = s.log_end();
        match s.set_refs(vec![(name.to_string(), vec![1; 16])]) {
            Err(StoreError::Decode(m)) => assert_eq!(
                m,
                format!("ref {name:?} is project metadata: only the store writes meta/ refs")
            ),
            other => panic!("{name}: {other:?}"),
        }
        assert_eq!(s.log_end(), end, "{name}: the log end does not move");
    }
}

#[test]
fn ec1_check_refuses_a_repo_id_that_is_not_16_bytes() {
    let (_d, mut s) = new_store();
    s.set_refs_unchecked_for_tests(vec![(REF_REPO_ID.into(), vec![7; 15])])
        .unwrap();
    assert_eq!(fsck_text(&s), "meta/repo_id holds 15 bytes, not 16");
}

#[test]
fn ec1_check_refuses_an_origin_that_does_not_decode() {
    let (_d, mut s) = new_store();
    s.set_refs_unchecked_for_tests(vec![(REF_ORIGIN.into(), vec![9])])
        .unwrap();
    assert_eq!(fsck_text(&s), "meta/origin does not decode");
}

#[test]
fn ec1_check_refuses_a_clone_origin_with_another_repo_id() {
    let (_d, mut s) = new_store();
    let r = s.reader().repo_id().unwrap().unwrap();
    let mut other = r;
    other.0[0] ^= 0xff;
    let o = Origin {
        fork: false,
        repo_id: other,
        tag: Some("v1".into()),
        landing: Some(Hash::of(b"head")),
    };
    assert_eq!(decode_origin(&encode_origin(&o)), Some(o.clone()));
    s.set_refs_unchecked_for_tests(vec![(REF_ORIGIN.into(), encode_origin(&o))])
        .unwrap();
    assert_eq!(
        fsck_text(&s),
        format!("meta/origin names repo {other} for a clone, but meta/repo_id is {r}")
    );
}

#[test]
fn ec1_check_refuses_a_fork_origin_with_its_own_repo_id() {
    let (_d, mut s) = new_store();
    let r = s.reader().repo_id().unwrap().unwrap();
    let o = Origin {
        fork: true,
        repo_id: r,
        tag: None,
        landing: None,
    };
    s.set_refs_unchecked_for_tests(vec![(REF_ORIGIN.into(), encode_origin(&o))])
        .unwrap();
    assert_eq!(
        fsck_text(&s),
        format!("meta/origin names this repo {r} as the repo it was forked from")
    );
}

#[test]
fn ec1_check_refuses_an_unknown_meta_ref() {
    let (_d, mut s) = new_store();
    // two unknown refs and a bad repo id: the unknown ref first in name order is reported (O22)
    s.set_refs_unchecked_for_tests(vec![
        ("meta/zz".into(), vec![1]),
        ("meta/a".into(), vec![1]),
        (REF_REPO_ID.into(), vec![1]),
    ])
    .unwrap();
    assert_eq!(
        fsck_text(&s),
        "\"meta/a\" is not a meta ref this rdy knows (meta/store_format, meta/repo_id, meta/origin)"
    );
    // a good origin and repo id check clean
    let (_e, mut t) = new_store();
    let r = t.reader().repo_id().unwrap().unwrap();
    let o = Origin {
        fork: false,
        repo_id: r,
        tag: None,
        landing: None,
    };
    t.set_refs_unchecked_for_tests(vec![(REF_ORIGIN.into(), encode_origin(&o))])
        .unwrap();
    t.check_objects().unwrap();
    assert_eq!(t.reader().origin().unwrap(), Some(o));
    let mut other = r;
    other.0[15] ^= 1;
    let f = Origin {
        fork: true,
        repo_id: other,
        tag: Some("v2".into()),
        landing: None,
    };
    t.set_refs_unchecked_for_tests(vec![(REF_ORIGIN.into(), encode_origin(&f))])
        .unwrap();
    t.check_objects().unwrap();
}

#[test]
fn ec1_the_dump_reads_store_format_12_with_the_kinds_of_11() {
    for pair in [(1, 10), (1, 11), (1, 12)] {
        assert!(READS.contains(&pair), "{pair:?}");
    }
    let kinds_11 = frame_kinds(1, 11).unwrap();
    assert_eq!(frame_kinds(1, 12), Some(kinds_11));
    let header = DumpHeader {
        store_format: 12,
        log_end: Location {
            segment: 0,
            offset: 0,
        },
        rdy: "test".into(),
    };
    let batch = |kind: u8, n: u64| {
        let payload = format!("frame {kind}").into_bytes();
        let hash = Hash::of(&payload);
        DumpBatch {
            start: Location {
                segment: 0,
                offset: n,
            },
            digest: batch_digest([&hash]),
            frames: vec![Frame {
                kind,
                hash,
                payload,
            }],
        }
    };
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    for (i, k) in kinds_11.iter().enumerate() {
        w.batch(&batch(*k, i as u64))
            .unwrap_or_else(|e| panic!("kind {k}: {e}"));
    }
    match w.batch(&batch(11, 99)) {
        Err(StoreError::Refused(m)) => assert_eq!(m, "a record of kind 11 cannot stand here"),
        other => panic!("{other:?}"),
    }
    let (_, totals) = w.finish().unwrap();
    assert_eq!(totals.batches, kinds_11.len() as u64);
}
