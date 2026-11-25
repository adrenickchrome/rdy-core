//! Counted fork origins (D-HX6BX, D-QHHFK, D-CDQF7): only a holder's creating batch supplies
//! the fork origins its lineage counts. They come in log order: the source's origins oldest
//! first, then the fork's own. A later `meta/origin` in a holder counts for nothing. A project
//! counts every fork origin its log records before the end it is asked for. These tests read
//! the sealed log and the store's helper, not only a reply.
use rdy_model::*;
use rdy_store::format::{REF_STORE_FEATURES, baseline_features};
use rdy_store::holder::{HOLDER_MODE, REF_STORE_MODE, StoreMode};
use rdy_store::ingest::Ingest;
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::meta::{Origin, REF_ORIGIN, REF_REPO_ID, encode_origin, encode_repo_id};
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError};
use std::path::{Path, PathBuf};

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

fn id(n: u8) -> RepoId {
    RepoId([n; 16])
}

/// A fork origin of repo `n`, at tag `tag` and a fixed landing when `tag` is given.
fn fork_of(n: u8, tag: Option<&str>) -> Origin {
    Origin {
        fork: true,
        repo_id: id(n),
        tag: tag.map(str::to_string),
        landing: tag.map(|_| Hash([n; 32])),
    }
}

/// The creation origins of the fork fixture: two ancestors, oldest first, then the immediate
/// source at its tag.
fn creation_origins() -> Vec<Origin> {
    vec![
        fork_of(1, None),
        fork_of(2, Some("v1.0.0")),
        fork_of(3, Some("v2.0.0")),
    ]
}

/// The repo id of the forked holder fixture.
const FORK: u8 = 9;

fn forked_holder() -> (tempfile::TempDir, PathBuf, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fork");
    let s = Store::create_holder_with_origins(&path, id(FORK), &creation_origins()).unwrap();
    (dir, path, s)
}

/// Ref frames salted as the store salts them: the i-th ref's `at` is `at` plus i.
fn ref_frames(at: Location, refs: &[(&str, &[u8])]) -> Vec<(u8, Vec<u8>)> {
    refs.iter()
        .enumerate()
        .map(|(i, (name, value))| {
            let u = RefUpdate {
                name: (*name).into(),
                value: value.to_vec(),
                at: Location {
                    segment: at.segment,
                    offset: at.offset + i as u64,
                },
            };
            (kinds::REF_UPDATE, postcard::to_allocvec(&u).unwrap())
        })
        .collect()
}

/// A raw write of one ref through the public `Store::write`.
fn write_ref(s: &mut Store, name: &str, value: &[u8]) {
    let items = ref_frames(s.log_end(), &[(name, value)]);
    s.write(items).unwrap();
}

/// The counted fork origins at the store's log end.
fn counted(s: &Store) -> Vec<Origin> {
    s.reader().counted_fork_origins_before(s.log_end()).unwrap()
}

/// Every sealed batch of the log: its start and its frames, each with its location, kind and
/// payload.
type Frames = Vec<(Location, u8, Vec<u8>)>;

fn batches(s: &Store) -> Vec<Frames> {
    let mut out: Vec<Frames> = vec![vec![]];
    s.for_each_frame_from(ORIGIN, s.log_end(), |loc, kind, _, payload, _| {
        if kind == kinds::BATCH_COMMIT {
            out.push(vec![]);
        } else {
            out.last_mut().unwrap().push((loc, kind, payload.to_vec()));
        }
        Ok(())
    })
    .unwrap();
    out.pop();
    out
}

fn ingest(src: &Store, dst: &Path) {
    let mut ing = Ingest::create(dst).unwrap();
    let mut n = 0;
    src.reader()
        .for_each_batch(src.log_end(), |b| {
            ing.batch(n, &b)?;
            n += 1;
            Ok(())
        })
        .unwrap();
    ing.finish().unwrap();
}

fn decode_text<T: std::fmt::Debug>(r: Result<T, StoreError>) -> String {
    match r {
        Err(StoreError::Decode(m)) => m,
        other => panic!("not a Decode refusal: {other:?}"),
    }
}

#[test]
fn hfp1_creation_origins_keep_log_order() {
    let (_d, _path, s) = forked_holder();
    let origins = creation_origins();
    // one creation batch: the format, the repo id and the mode, then the origins in order
    let b = batches(&s);
    assert_eq!(b.len(), 1, "one creation batch");
    let refs: Vec<RefUpdate> = b[0]
        .iter()
        .map(|(_, k, p)| {
            assert_eq!(*k, kinds::REF_UPDATE);
            postcard::from_bytes(p).unwrap()
        })
        .collect();
    let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        [
            REF_STORE_FEATURES,
            REF_REPO_ID,
            REF_STORE_MODE,
            REF_ORIGIN,
            REF_ORIGIN,
            REF_ORIGIN
        ]
    );
    assert_eq!(refs[0].value, baseline_features().to_json());
    assert_eq!(refs[1].value, encode_repo_id(&id(FORK)));
    assert_eq!(refs[2].value, HOLDER_MODE);
    let values: Vec<Vec<u8>> = refs[3..].iter().map(|r| r.value.clone()).collect();
    let want: Vec<Vec<u8>> = origins.iter().map(encode_origin).collect();
    assert_eq!(values, want);
    // the fixture's object hash order is not its log order, so an object walk cannot pass
    let mut by_hash: Vec<(Hash, usize)> = b[0][3..]
        .iter()
        .enumerate()
        .map(|(i, (_, _, p))| (Hash::of(p), i))
        .collect();
    by_hash.sort();
    assert_ne!(
        by_hash.iter().map(|(_, i)| *i).collect::<Vec<_>>(),
        [0, 1, 2],
        "fixture: hash order must differ from log order"
    );

    // the helper reads them in log order; the current value is the fork's own origin
    assert_eq!(counted(&s), origins);
    assert_eq!(s.origin().unwrap(), Some(origins[2].clone()));
    assert_eq!(s.mode().unwrap(), StoreMode::Holder);
    assert_eq!(s.repo_id().unwrap(), Some(id(FORK)));
    s.check_meta().unwrap();

    // bounded by the end it is given: the origin, and a point inside the creating batch
    assert!(
        s.reader()
            .counted_fork_origins_before(ORIGIN)
            .unwrap()
            .is_empty()
    );
    let after_first = b[0][4].0;
    assert_eq!(
        s.reader().counted_fork_origins_before(after_first).unwrap(),
        origins[..1]
    );

    // an ordinary holder writes no origin and counts none
    let dir = tempfile::tempdir().unwrap();
    let plain = Store::create_holder(&dir.path().join("h"), id(7)).unwrap();
    assert_eq!(plain.origin().unwrap(), None);
    assert!(counted(&plain).is_empty());
    let names: Vec<String> = batches(&plain)[0]
        .iter()
        .map(|(_, _, p)| postcard::from_bytes::<RefUpdate>(p).unwrap().name)
        .collect();
    assert_eq!(names, [REF_STORE_FEATURES, REF_REPO_ID, REF_STORE_MODE]);
    // a holder of no origins is an ordinary holder
    let none = Store::create_holder_with_origins(&dir.path().join("n"), id(7), &[]).unwrap();
    assert_eq!(batches(&none), batches(&plain));

    // a project counts every fork origin its log records before the end, in log order; a
    // clone's origin and a value that does not decode count for nothing
    let mut p = Store::create(&dir.path().join("p")).unwrap();
    let own = p.repo_id().unwrap().unwrap();
    assert!(counted(&p).is_empty());
    let first = fork_of(5, Some("v1.0.0"));
    p.put_origin(&first, Some(id(6))).unwrap();
    let between = p.log_end();
    let clone = Origin {
        fork: false,
        repo_id: id(6),
        tag: None,
        landing: None,
    };
    p.put_origin(&clone, None).unwrap();
    write_ref(&mut p, REF_ORIGIN, &[0xff; 3]);
    let second = fork_of(9, None);
    p.put_origin(&second, Some(id(8))).unwrap();
    let third = fork_of(10, Some("v2.0.0"));
    p.put_origin(&third, Some(id(11))).unwrap();
    assert_ne!(own, id(6));
    assert_eq!(counted(&p), [first.clone(), second, third]);
    assert_eq!(
        p.reader().counted_fork_origins_before(between).unwrap(),
        [first]
    );
    // the fixture's fork origin updates are not in object hash order either
    let mut by_hash: Vec<(Hash, usize)> = batches(&p)
        .iter()
        .flatten()
        .filter(|(_, kind, payload)| {
            if *kind != kinds::REF_UPDATE {
                return false;
            }
            let u: RefUpdate = postcard::from_bytes(payload).unwrap();
            u.name == REF_ORIGIN && rdy_store::meta::decode_origin(&u.value).is_some_and(|o| o.fork)
        })
        .enumerate()
        .map(|(i, (_, _, payload))| (Hash::of(payload), i))
        .collect();
    by_hash.sort();
    assert_ne!(
        by_hash.iter().map(|(_, i)| *i).collect::<Vec<_>>(),
        [0, 1, 2],
        "fixture: hash order must differ from log order"
    );
}

#[test]
fn hfp1_later_origins_widen_nothing() {
    // a normal holder takes a later valid origin through the store's own origin batch and a
    // raw write: it counts none of them
    let dir = tempfile::tempdir().unwrap();
    let mut h = Store::create_holder(&dir.path().join("h"), id(7)).unwrap();
    let later = fork_of(2, Some("v1.0.0"));
    h.put_origin(&later, None).unwrap();
    write_ref(&mut h, REF_ORIGIN, &encode_origin(&fork_of(3, None)));
    assert_eq!(h.origin().unwrap(), Some(fork_of(3, None)));
    h.check_meta().unwrap();
    assert!(counted(&h).is_empty());

    // a forked holder takes later origins, one a repeat of a creation value: they neither
    // widen nor erase its lineage
    let (_d, path, mut s) = forked_holder();
    let origins = creation_origins();
    let other = fork_of(5, Some("v3.0.0"));
    s.put_origin(&other, None).unwrap();
    s.put_origin(&origins[0], None).unwrap();
    write_ref(&mut s, REF_ORIGIN, &encode_origin(&fork_of(6, None)));
    assert_eq!(batches(&s).len(), 4);
    assert_eq!(s.origin().unwrap(), Some(fork_of(6, None)));
    s.check_meta().unwrap();
    assert_eq!(counted(&s), origins);

    // a later value that does not decode is ignored for lineage; the fsck still reports it
    write_ref(&mut s, REF_ORIGIN, &[0xff; 3]);
    assert_eq!(counted(&s), origins);
    assert_eq!(decode_text(s.check_meta()), "meta/origin does not decode");
    drop(s);
    let s = Store::open(&path).unwrap();
    assert_eq!(counted(&s), origins);
}

#[test]
fn hfp1_reopen_reindex_and_load_keep_origins() {
    let (d, path, mut s) = forked_holder();
    let origins = creation_origins();
    s.put_origin(&fork_of(5, None), None).unwrap();
    let end = s.log_end();
    assert_eq!(counted(&s), origins);
    drop(s);

    // a reopen and a reindex read the same log
    let mut s = Store::open(&path).unwrap();
    assert_eq!(s.log_end(), end);
    assert_eq!(counted(&s), origins);
    s.reindex().unwrap();
    assert_eq!(counted(&s), origins);
    assert_eq!(s.origin().unwrap(), Some(fork_of(5, None)));
    s.check_meta().unwrap();

    // a load of its dump keeps the creating batch, so it keeps the same counted origins
    let copy = d.path().join("copy");
    ingest(&s, &copy);
    drop(s);
    let c = Store::open(&copy).unwrap();
    assert_eq!(c.mode().unwrap(), StoreMode::Holder);
    assert_eq!(c.repo_id().unwrap(), Some(id(FORK)));
    assert_eq!(c.log_end(), end);
    assert_eq!(counted(&c), origins);
    assert_eq!(c.origin().unwrap(), Some(fork_of(5, None)));
    c.check_meta().unwrap();
}
