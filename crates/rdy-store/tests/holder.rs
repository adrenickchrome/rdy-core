//! Holder stores (D-BPYN8, D-DAGNB): `meta/store_mode` names a holder in the first sealed batch,
//! its absence names a project, and a holder holds only meta refs, tags and the answers that
//! relay them (O11). These tests read the refs and the sealed log, not only a reply.
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_store::dump::{DumpBatch, Frame};
use rdy_store::format::{FORMAT_FILE, Format, REF_STORE_FORMAT, STORE_FORMAT, encode_u32};
use rdy_store::holder::{HOLDER_MODE, REF_STORE_MODE, StoreMode};
use rdy_store::ingest::Ingest;
use rdy_store::kinds::{self, RefUpdate, batch_digest};
use rdy_store::meta::{REF_ORIGIN, REF_REPO_ID, encode_repo_id};
use rdy_store::segment::{Location, SegmentLog};
use rdy_store::{Store, StoreError, StoreLock};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};
const SEGMENT_MAX: u64 = 64 * 1024 * 1024;
const LATE: &str = "holder mode must be set in the first batch";
const BAD_MODE: &str = "meta/store_mode must be holder when present";
const CHANGE_MODE: &str = "store mode cannot change";

fn id(n: u8) -> RepoId {
    RepoId([n; 16])
}

fn holder() -> (tempfile::TempDir, PathBuf, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("h");
    let s = Store::create_holder(&path, id(7)).unwrap();
    (dir, path, s)
}

fn project() -> (tempfile::TempDir, PathBuf, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p");
    let s = Store::create(&path).unwrap();
    (dir, path, s)
}

fn prov() -> Provenance {
    Provenance {
        actor: "human:a".into(),
        run: None,
        role: Role::Approver,
        at_ms: 5,
    }
}

fn tag(name: &str) -> Tag {
    Tag {
        name: name.into(),
        landing: Hash([3; 32]),
        message: None,
        tagged_by: prov(),
    }
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

/// Appends one sealed batch of `refs` to the closed store at `dir`, as raw log bytes (no store
/// writer and none of its guards). Returns the batch's start.
fn append_raw(dir: &Path, refs: &[(&str, &[u8])]) -> Location {
    let (mut log, quarantined) =
        SegmentLog::open_marked(&dir.join("segments"), SEGMENT_MAX, None).unwrap();
    assert!(quarantined.is_empty());
    let start = log.end();
    log.append_batch(&ref_frames(start, refs)).unwrap();
    log.sync().unwrap();
    start
}

/// A store directory whose first sealed batch is `refs`, with this rdy's `FORMAT` file.
fn raw_store(dir: &Path, refs: &[(&str, &[u8])]) {
    std::fs::create_dir(dir).unwrap();
    std::fs::create_dir(dir.join("segments")).unwrap();
    append_raw(dir, refs);
    rdy_store::format::write_format_file(dir, Format::CURRENT).unwrap();
}

fn decode_text<T>(r: Result<T, StoreError>) -> String {
    match r {
        Err(StoreError::Decode(m)) => m,
        Err(e) => panic!("not a Decode refusal: {e:?}"),
        Ok(_) => panic!("not refused"),
    }
}

fn refused_text<T>(r: Result<T, StoreError>) -> String {
    match r {
        Err(StoreError::Refused(m)) => m,
        Err(e) => panic!("not Refused: {e:?}"),
        Ok(_) => panic!("not refused"),
    }
}

/// Every ref of the store, by name.
fn all_refs(s: &Store) -> BTreeMap<String, Vec<u8>> {
    s.refs_with_prefix("").unwrap().into_iter().collect()
}

/// The sealed log's batches, each as its frames' (kind, payload), payloads all present.
/// A sealed batch: its start and its frames' (kind, payload).
type Batch = (Location, Vec<(u8, Vec<u8>)>);

fn batches(s: &Store) -> Vec<Batch> {
    let mut out = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            let frames = b
                .frames
                .iter()
                .map(|f| (f.kind(), f.present_payload().unwrap().to_vec()))
                .collect();
            out.push((b.start, frames));
            Ok(())
        })
        .unwrap();
    out
}

fn ref_update(payload: &[u8]) -> RefUpdate {
    postcard::from_bytes(payload).unwrap()
}

/// A raw write of one ref through the public `Store::write`.
fn write_ref(s: &mut Store, name: &str, value: &[u8]) -> Result<Vec<Hash>, StoreError> {
    let items = ref_frames(s.log_end(), &[(name, value)]);
    s.write(items)
}

/// The store's batches as a dump carries them, in log order.
fn dump_batches(s: &Store) -> Vec<DumpBatch> {
    let mut out = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            out.push(b);
            Ok(())
        })
        .unwrap();
    out
}

/// A dump batch of `refs` at `start`.
fn ref_batch(start: Location, refs: &[(&str, &[u8])]) -> DumpBatch {
    let frames: Vec<Frame> = ref_frames(start, refs)
        .into_iter()
        .map(|(kind, payload)| Frame::Present {
            kind,
            hash: Hash::of(&payload),
            payload,
        })
        .collect();
    DumpBatch {
        start,
        digest: batch_digest(frames.iter().map(Frame::hash)),
        frames,
    }
}

/// Ingests `src`'s batches then `extra` (built at the log's end) into a new store `dst`; the
/// first refusal, if any.
fn ingest_with(src: &Store, dst: &Path, extra: Option<&[(&str, &[u8])]>) -> Result<(), StoreError> {
    let mut ing = Ingest::create(dst)?;
    for (n, b) in dump_batches(src).iter().enumerate() {
        ing.batch(n as u64, b)?;
    }
    if let Some(refs) = extra {
        let b = ref_batch(ing.end(), refs);
        ing.batch(99, &b)?;
    }
    ing.finish().map(|_| ())
}

#[test]
fn ha4_holder_has_identity_and_no_canon() {
    let (_d, path, s) = holder();
    assert_eq!(s.mode().unwrap(), StoreMode::Holder);
    assert_eq!(s.reader().mode().unwrap(), StoreMode::Holder);
    assert_eq!(s.repo_id().unwrap(), Some(id(7)));
    assert_eq!(s.origin().unwrap(), None, "no origin is invented");
    assert_eq!(s.canon_head().unwrap(), None, "a holder has no canon");
    let refs = all_refs(&s);
    let names: Vec<&str> = refs.keys().map(String::as_str).collect();
    assert_eq!(names, [REF_REPO_ID, REF_STORE_FORMAT, REF_STORE_MODE]);
    assert_eq!(refs[REF_STORE_MODE], b"holder");
    assert_eq!(HOLDER_MODE, b"holder");
    assert_eq!(refs[REF_STORE_FORMAT], encode_u32(STORE_FORMAT));
    assert_eq!(
        rdy_store::format::read_format_file(&path).unwrap(),
        Some(Format::CURRENT)
    );
    s.check_meta().unwrap();
    // the fresh holder owns the store lock
    assert!(matches!(StoreLock::take(&path), Err(StoreError::Locked(_))));
    // a tag is a holder's own content
    let mut s = s;
    let t = s.put_tag(&tag("v1.0.0")).unwrap();
    assert_eq!(s.get_ref("tag/v1.0.0").unwrap(), Some(t.0.to_vec()));
    s.check_meta().unwrap();
    drop(s);
    // reopen: the same identity, from the log
    let s = Store::open(&path).unwrap();
    assert_eq!(s.mode().unwrap(), StoreMode::Holder);
    assert_eq!(s.repo_id().unwrap(), Some(id(7)));
    assert_eq!(s.canon_head().unwrap(), None);
    drop(s);
    // a directory that holds anything is never made a holder
    assert!(matches!(
        Store::create_holder(&path, id(8)),
        Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists
    ));
    let s = Store::open(&path).unwrap();
    assert_eq!(s.repo_id().unwrap(), Some(id(7)));
}

#[test]
fn ha4_project_mode_is_the_absent_ref() {
    let (_d, path, s) = project();
    assert_eq!(s.mode().unwrap(), StoreMode::Project);
    assert_eq!(s.get_ref(REF_STORE_MODE).unwrap(), None);
    // the first batch keeps its shape: the format (now 15) and the repo id, nothing more
    let b = batches(&s);
    assert_eq!(b.len(), 1);
    let refs: Vec<RefUpdate> = b[0].1.iter().map(|(_, p)| ref_update(p)).collect();
    let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, [REF_STORE_FORMAT, REF_REPO_ID]);
    assert_eq!(refs[0].value, b"15");
    assert_eq!(STORE_FORMAT, 15);
    s.check_meta().unwrap();
    drop(s);
    let s = Store::open(&path).unwrap();
    assert_eq!(s.mode().unwrap(), StoreMode::Project);
}

#[test]
fn ha4_bad_mode_and_holder_canon_fail_open_and_fsck() {
    // a first batch whose mode is not `holder` fails open, and the value fails mode()
    for bad in [&b"project"[..], b"", b"Holder", b"holder\n"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        raw_store(
            &path,
            &[
                (REF_STORE_FORMAT, &encode_u32(STORE_FORMAT)),
                (REF_REPO_ID, &encode_repo_id(&id(1))),
                (REF_STORE_MODE, bad),
            ],
        );
        assert_eq!(decode_text(Store::open(&path)), BAD_MODE, "{bad:?}");
    }
    // a project with a forged mode value: mode() and fsck refuse it, and so does the next open
    let (_d, path, mut s) = project();
    s.set_refs_unchecked_for_tests(vec![(REF_STORE_MODE.into(), b"nope".to_vec())])
        .unwrap();
    assert_eq!(decode_text(s.mode()), BAD_MODE);
    assert_eq!(decode_text(s.check_objects()), LATE);
    drop(s);
    assert_eq!(decode_text(Store::open(&path)), LATE);

    // a holder with a forged canon: the meta fsck refuses it (the project fsck fails on it too:
    // its holder branch is a later task's), and so do the next open and a reindex
    let (_d, path, mut s) = holder();
    s.set_refs_unchecked_for_tests(vec![("canon".into(), vec![9; 32])])
        .unwrap();
    assert_eq!(
        decode_text(s.check_meta()),
        "holder ref canon is not allowed"
    );
    assert!(s.check_objects().is_err());
    assert_eq!(decode_text(s.reindex()), "holder ref canon is not allowed");
    drop(s);
    assert_eq!(
        decode_text(Store::open(&path)),
        "holder ref canon is not allowed"
    );
    // the same canon forged as raw log bytes fails open with no index to trust
    let (_d, path, s) = holder();
    drop(s);
    append_raw(&path, &[("canon", &[9; 32])]);
    std::fs::remove_file(path.join("index.redb")).unwrap();
    assert_eq!(
        decode_text(Store::open(&path)),
        "holder ref canon is not allowed"
    );
}

#[test]
fn ha4_mode_is_not_a_public_ref_write() {
    // a project: set_refs and a raw write both refuse the mode as a mode change
    let (_d, path, mut s) = project();
    let end = s.log_end();
    assert_eq!(
        refused_text(s.set_refs(vec![(REF_STORE_MODE.into(), b"holder".to_vec())])),
        CHANGE_MODE
    );
    assert_eq!(
        refused_text(write_ref(&mut s, REF_STORE_MODE, b"holder")),
        CHANGE_MODE
    );
    // nor can a held batch's seal carry it
    s.set_held(true).unwrap();
    s.put_tag(&tag("v1.0.0")).unwrap();
    let last = ref_frames(s.log_end(), &[(REF_STORE_MODE, b"holder")]);
    assert_eq!(refused_text(s.seal(last)), CHANGE_MODE);
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    assert_eq!(s.mode().unwrap(), StoreMode::Project);
    assert!(s.log_end() > end, "the tag's held batch sealed");
    assert!(
        batches(&s).iter().all(|(_, f)| f
            .iter()
            .filter(|(k, _)| *k == kinds::REF_UPDATE)
            .all(|(_, p)| ref_update(p).name != REF_STORE_MODE)),
        "no mode ref reached the log"
    );
    drop(s);
    assert_eq!(
        Store::open(&path).unwrap().mode().unwrap(),
        StoreMode::Project
    );

    // a holder: neither changing nor removing the mode is a write
    let (_d, _path, mut s) = holder();
    let end = s.log_end();
    for value in [&b"holder"[..], b"", b"project"] {
        assert_eq!(
            refused_text(write_ref(&mut s, REF_STORE_MODE, value)),
            CHANGE_MODE
        );
    }
    assert_eq!(s.log_end(), end, "a refused write writes nothing");
    assert_eq!(s.mode().unwrap(), StoreMode::Holder);
}

#[test]
fn ha4_reindex_preserves_holder_mode() {
    let (_d, path, mut s) = holder();
    s.put_tag(&tag("v1.0.0")).unwrap();
    s.put_tag(&tag("v2.0.0")).unwrap();
    let refs = all_refs(&s);
    let tables = s.index_dump_for_tests().unwrap();
    s.reindex().unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), tables);
    assert_eq!(s.mode().unwrap(), StoreMode::Holder);
    assert_eq!(all_refs(&s), refs);
    drop(s);
    // with no index at all, the open rebuilds the holder from the log
    std::fs::remove_file(path.join("index.redb")).unwrap();
    let s = Store::open(&path).unwrap();
    assert!(s.open_report().caught_up_frames > 0);
    assert_eq!(s.mode().unwrap(), StoreMode::Holder);
    assert_eq!(s.repo_id().unwrap(), Some(id(7)));
    assert_eq!(all_refs(&s), refs);
    assert_eq!(s.index_dump_for_tests().unwrap(), tables);
}

#[test]
fn ha4_holder_creation_is_one_durable_batch() {
    let (_d, path, s) = holder();
    let b = batches(&s);
    assert_eq!(b.len(), 1, "one creation batch");
    assert_eq!(b[0].0, ORIGIN);
    let refs: Vec<RefUpdate> = b[0]
        .1
        .iter()
        .map(|(k, p)| {
            assert_eq!(*k, kinds::REF_UPDATE);
            ref_update(p)
        })
        .collect();
    let got: Vec<(&str, &[u8], Location)> = refs
        .iter()
        .map(|r| (r.name.as_str(), &r.value[..], r.at))
        .collect();
    let at = |offset| Location { segment: 0, offset };
    let fmt = encode_u32(STORE_FORMAT);
    let rid = encode_repo_id(&id(7));
    assert_eq!(
        got,
        [
            (REF_STORE_FORMAT, &fmt[..], at(0)),
            (REF_REPO_ID, &rid[..], at(1)),
            (REF_STORE_MODE, &b"holder"[..], at(2)),
        ]
    );
    // durable: the index holds the batch durably, and the creation is not a caller's write
    assert_eq!(s.pending_checkpoint_frames(), 0);
    assert!(s.last_batch().is_empty());
    assert!(path.join(FORMAT_FILE).exists());
    assert!(s.durable_dirs().contains(&path));
    let end = s.log_end();
    drop(s);
    let s = Store::open(&path).unwrap();
    assert_eq!(s.log_end(), end, "the open adds no batch");
    assert_eq!(batches(&s).len(), 1);
    assert_eq!(s.mode().unwrap(), StoreMode::Holder);
    assert_eq!(s.open_report().caught_up_frames, 0);
}

#[test]
fn ha4_holder_ref_families_are_closed() {
    let forbidden = [
        "canon",
        "view/work",
        "proposal/p-1",
        "open/p-1",
        "pending/human:a/q-1",
        "inbox/agent:a",
        "intent/i-1",
        "alt/a-1",
        "answer/q-1",
        "answer/tagged",
        "dismiss/k",
        "mention/b/e",
        "pair/k",
        "semantic/k",
        "meta/zz",
        "other",
    ];
    // a public write of each is refused, and writes nothing
    let (_d, _path, mut s) = holder();
    let end = s.log_end();
    for name in forbidden.iter().copied().chain(["answer/tag:v1"]) {
        assert_eq!(
            refused_text(write_ref(&mut s, name, &[1; 32])),
            format!("a holder store cannot write ref {name}"),
        );
    }
    // the answer ref is refused even beside its tag in the same write
    let t = s.put_tag(&tag("v1")).unwrap();
    let items = ref_frames(
        s.log_end(),
        &[("tag/v2", &t.0), ("answer/tag:v2", &[1; 32])],
    );
    assert_eq!(
        refused_text(s.write(items)),
        "a holder store cannot write ref answer/tag:v2"
    );
    assert_eq!(
        s.get_ref("tag/v2").unwrap(),
        None,
        "nothing of it was written"
    );
    assert!(s.log_end() > end);
    s.check_meta().unwrap();
    drop(s);

    // forged into the log, each fails the open
    for name in forbidden {
        let (_d, path, s) = holder();
        drop(s);
        append_raw(&path, &[(name, &[1; 32])]);
        assert_eq!(
            decode_text(Store::open(&path)),
            format!("holder ref {name} is not allowed"),
        );
    }
    // the answer ref needs its tag, in the same batch or an earlier one
    let ans: &[u8] = &[4; 32];
    let tagv: &[u8] = &[5; 32];
    for (case, batches_of, ok) in [
        (
            "same batch",
            vec![vec![("tag/v1", tagv), ("answer/tag:v1", ans)]],
            true,
        ),
        (
            "answer first",
            vec![vec![("answer/tag:v1", ans), ("tag/v1", tagv)]],
            true,
        ),
        (
            "earlier batch",
            vec![vec![("tag/v1", tagv)], vec![("answer/tag:v1", ans)]],
            true,
        ),
        ("no tag", vec![vec![("answer/tag:v1", ans)]], false),
        (
            "another tag",
            vec![vec![("tag/v2", tagv), ("answer/tag:v1", ans)]],
            false,
        ),
        (
            "later tag",
            vec![vec![("answer/tag:v1", ans)], vec![("tag/v1", tagv)]],
            false,
        ),
    ] {
        let (_d, path, s) = holder();
        drop(s);
        for b in &batches_of {
            append_raw(&path, b);
        }
        // with the index and without it
        for fresh in [false, true] {
            if fresh {
                let _ = std::fs::remove_file(path.join("index.redb"));
            }
            let r = Store::open(&path);
            if ok {
                let s = r.unwrap_or_else(|e| panic!("{case}: {e:?}"));
                assert_eq!(s.get_ref("answer/tag:v1").unwrap(), Some(ans.to_vec()));
                s.check_meta().unwrap();
            } else {
                assert_eq!(
                    decode_text(r),
                    "holder ref answer/tag:v1 is not allowed",
                    "{case}"
                );
            }
        }
    }
    // fsck: an answer ref forged beside no tag
    let (_d, _path, mut s) = holder();
    s.set_refs_unchecked_for_tests(vec![("answer/tag:v9".into(), ans.to_vec())])
        .unwrap();
    assert_eq!(
        decode_text(s.check_meta()),
        "holder ref answer/tag:v9 is not allowed"
    );
    // ingest: a load of a holder dump with a forged canon is refused
    let (d, _path, s) = holder();
    assert_eq!(
        decode_text(ingest_with(
            &s,
            &d.path().join("copy"),
            Some(&[("canon", &[1; 32])])
        )),
        "holder ref canon is not allowed"
    );
    ingest_with(&s, &d.path().join("ok"), None).unwrap();
    let copy = Store::open(&d.path().join("ok")).unwrap();
    assert_eq!(copy.mode().unwrap(), StoreMode::Holder);
    assert_eq!(copy.repo_id().unwrap(), Some(id(7)));
}

#[test]
fn ha4_late_mode_cannot_reinterpret_native_redactions() {
    // a project with a native, applied removal
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let b = Block {
        id: BlockId::parse("b-00001").unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: "canary alpha".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    };
    let change = Change {
        view: "work".into(),
        ops: vec![Op::Create { block: b.clone() }],
        deps: vec![],
        provenance: Provenance {
            actor: "agent:w".into(),
            run: Some("w1".into()),
            role: Role::Writer,
            at_ms: 10,
        },
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    s.commit_to_view("work", change).unwrap();
    drop(s);
    let fx = authorized_batch_fixture(
        &path,
        &[RequestSpec::new(
            "work",
            vec![RedactTarget::Version {
                id: b.id.clone(),
                version: b.version_hash(),
            }],
        )],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    let removed = fx.requests[0].request;
    let mut s = Store::open(&path).unwrap();
    assert_eq!(
        s.redaction_of(kinds::BLOCK, &b.version_hash()).unwrap(),
        Some(removed)
    );
    assert_eq!(s.mode().unwrap(), StoreMode::Project);

    // write: a late insertion is refused, and nothing is written
    let end = s.log_end();
    assert_eq!(
        refused_text(write_ref(&mut s, REF_STORE_MODE, HOLDER_MODE)),
        CHANGE_MODE
    );
    assert_eq!(s.log_end(), end);

    // ingest: a load whose log inserts the mode late is refused
    assert_eq!(
        decode_text(ingest_with(
            &s,
            &dir.path().join("late"),
            Some(&[(REF_STORE_MODE, HOLDER_MODE)])
        )),
        LATE
    );
    ingest_with(&s, &dir.path().join("plain"), None).unwrap();
    assert_eq!(
        Store::open(&dir.path().join("plain"))
            .unwrap()
            .mode()
            .unwrap(),
        StoreMode::Project
    );

    // reindex: a forged late insertion is refused, and fsck refuses it too
    s.set_refs_unchecked_for_tests(vec![(REF_STORE_MODE.into(), HOLDER_MODE.to_vec())])
        .unwrap();
    assert_eq!(decode_text(s.check_objects()), LATE);
    assert_eq!(decode_text(s.reindex()), LATE);
    drop(s);
    // open: refused from the index, and from the log alone
    assert_eq!(decode_text(Store::open(&path)), LATE);
    std::fs::remove_file(path.join("index.redb")).unwrap();
    assert_eq!(decode_text(Store::open(&path)), LATE);

    // the raw log route on a clean copy: a late insertion fails the open
    let (_d, ppath, p) = project();
    drop(p);
    append_raw(&ppath, &[(REF_STORE_MODE, HOLDER_MODE)]);
    assert_eq!(decode_text(Store::open(&ppath)), LATE);

    // a holder: late deletion and replacement fail write, open, reindex and ingest
    for value in [&b""[..], b"project", b"holder"] {
        let (d, hpath, mut h) = holder();
        assert_eq!(
            refused_text(write_ref(&mut h, REF_STORE_MODE, value)),
            CHANGE_MODE
        );
        assert_eq!(
            decode_text(ingest_with(
                &h,
                &d.path().join("copy"),
                Some(&[(REF_STORE_MODE, value)])
            )),
            LATE,
            "{value:?}"
        );
        h.set_refs_unchecked_for_tests(vec![(REF_STORE_MODE.into(), value.to_vec())])
            .unwrap();
        assert_eq!(decode_text(h.reindex()), LATE, "{value:?}");
        drop(h);
        std::fs::remove_file(hpath.join("index.redb")).unwrap();
        assert_eq!(decode_text(Store::open(&hpath)), LATE, "{value:?}");
        // from a raw log with a good index: the catch-up refuses it
        let (_e, rpath, r) = holder();
        drop(r);
        append_raw(&rpath, &[(REF_STORE_MODE, value)]);
        assert_eq!(decode_text(Store::open(&rpath)), LATE, "{value:?}");
    }
}

/// Appends one sealed raw batch of `frames` (kind, payload) to the closed store at `dir`.
fn append_raw_frames(dir: &Path, frames: &[(u8, Vec<u8>)]) {
    let (mut log, quarantined) =
        SegmentLog::open_marked(&dir.join("segments"), SEGMENT_MAX, None).unwrap();
    assert!(quarantined.is_empty());
    log.append_batch(frames).unwrap();
    log.sync().unwrap();
}

#[test]
fn ha4_a_warm_index_open_and_fsck_read_the_log_history() {
    let ans: &[u8] = &[4; 32];
    // a late, identical mode write: the refs equal the creation's, the log does not
    let (_d, path, mut s) = holder();
    s.set_refs_unchecked_for_tests(vec![(REF_STORE_MODE.into(), HOLDER_MODE.to_vec())])
        .unwrap();
    assert_eq!(all_refs(&s)[REF_STORE_MODE], HOLDER_MODE);
    assert_eq!(decode_text(s.check_meta()), LATE);
    s.checkpoint().unwrap();
    drop(s);
    assert_eq!(decode_text(Store::open(&path)), LATE);
    assert_eq!(
        decode_text(Store::open(&path)),
        LATE,
        "a retry stays refused"
    );

    // an answer ref in a batch before its tag's: the refs pair up, the log does not
    let (_d, path, mut s) = holder();
    s.set_refs_unchecked_for_tests(vec![("answer/tag:v1".into(), ans.to_vec())])
        .unwrap();
    s.put_tag(&tag("v1")).unwrap();
    assert!(s.get_ref("tag/v1").unwrap().is_some());
    assert_eq!(
        decode_text(s.check_meta()),
        "holder ref answer/tag:v1 is not allowed"
    );
    s.checkpoint().unwrap();
    drop(s);
    for _ in 0..2 {
        assert_eq!(
            decode_text(Store::open(&path)),
            "holder ref answer/tag:v1 is not allowed"
        );
    }

    // the control: the tag first, then its answer, opens warm and passes fsck
    let (_d, path, mut s) = holder();
    s.put_tag(&tag("v1")).unwrap();
    s.set_refs_unchecked_for_tests(vec![("answer/tag:v1".into(), ans.to_vec())])
        .unwrap();
    s.check_meta().unwrap();
    s.checkpoint().unwrap();
    drop(s);
    let s = Store::open(&path).unwrap();
    assert_eq!(s.open_report().caught_up_frames, 0, "the index was warm");
    assert_eq!(s.get_ref("answer/tag:v1").unwrap(), Some(ans.to_vec()));
    s.check_meta().unwrap();
}

#[test]
fn ha4_a_failed_catch_up_keeps_its_batch_refused() {
    let ans: &[u8] = &[4; 32];
    let chunk = rdy_store::CATCH_UP_CHUNK;
    // the replay starts past the creation batch: the answer is its first frame, so a chunk
    // ends inside the batch for every count from `chunk` - 1 on
    for chunks in [chunk - 2, chunk - 1, chunk, chunk + 1, 1100, 2 * chunk + 5] {
        let (_d, path, s) = holder();
        let start = s.log_end();
        drop(s);
        let mut frames = ref_frames(start, &[("answer/tag:v1", ans)]);
        frames.extend((0..chunks as u32).map(|i| (kinds::BLOB_CHUNK, i.to_le_bytes().to_vec())));
        append_raw_frames(&path, &frames);
        append_raw(&path, &[("tag/v1", &[5; 32])]);
        // every open is refused: a failed catch-up never forgets the batch it stopped in
        for attempt in 0..3 {
            let r = Store::open(&path).map(|_| ());
            assert!(
                matches!(&r, Err(StoreError::Decode(m)) if m == "holder ref answer/tag:v1 is not allowed"),
                "{chunks} chunk frames, open {attempt}: {r:?}"
            );
        }
        // and with no index at all
        let _ = std::fs::remove_file(path.join("index.redb"));
        assert_eq!(
            decode_text(Store::open(&path)),
            "holder ref answer/tag:v1 is not allowed",
            "{chunks} chunk frames, fresh index"
        );
    }
}

#[test]
fn ha4_set_refs_takes_the_mode_and_holder_refusals_first() {
    // a project: the mode ref is the mode refusal; other refs keep their own refusals
    let (_d, _path, mut s) = project();
    let end = s.log_end();
    assert_eq!(
        refused_text(s.set_refs(vec![(REF_STORE_MODE.into(), HOLDER_MODE.to_vec())])),
        CHANGE_MODE
    );
    assert_eq!(
        decode_text(s.set_refs(vec![("meta/zz".into(), vec![1])])),
        "ref \"meta/zz\" is project metadata: only the store writes meta/ refs"
    );
    assert_eq!(
        decode_text(s.set_refs(vec![("answer/tag:v1".into(), vec![1; 32])])),
        "ref \"answer/tag:v1\" is an answer: only put_answer writes one"
    );
    assert_eq!(s.log_end(), end, "nothing was written");

    // a holder: the mode refusal, then the holder ref refusal, whatever set_refs refused before
    let (_d, _path, mut s) = holder();
    let end = s.log_end();
    for value in [&b"holder"[..], b"", b"project"] {
        assert_eq!(
            refused_text(s.set_refs(vec![(REF_STORE_MODE.into(), value.to_vec())])),
            CHANGE_MODE
        );
    }
    for name in [
        "answer/tag:v1",
        "answer/q-1",
        "pending/human:a/q-1",
        "inbox/agent:a",
        "dismiss/k",
        "mention/b/e",
        "pair/k",
        "semantic/k",
        "canon",
        "view/work",
        "other",
    ] {
        let want = format!("a holder store cannot write ref {name}");
        assert_eq!(
            refused_text(s.set_refs(vec![(name.into(), vec![1; 32])])),
            want,
            "set_refs"
        );
        assert_eq!(
            refused_text(write_ref(&mut s, name, &[1; 32])),
            want,
            "write"
        );
    }
    // a meta ref the holder allows keeps the store-only refusal
    assert_eq!(
        decode_text(s.set_refs(vec![(REF_ORIGIN.into(), vec![1])])),
        "ref \"meta/origin\" is project metadata: only the store writes meta/ refs"
    );
    assert_eq!(s.log_end(), end, "nothing was written");
    // a held batch's seal: the same exact refusals
    s.set_held(true).unwrap();
    s.put_tag(&tag("v1")).unwrap();
    for (name, want) in [
        (REF_STORE_MODE, CHANGE_MODE.to_string()),
        (
            "answer/tag:v1",
            "a holder store cannot write ref answer/tag:v1".to_string(),
        ),
        ("canon", "a holder store cannot write ref canon".to_string()),
    ] {
        let last = ref_frames(s.log_end(), &[(name, &[1; 32])]);
        assert_eq!(refused_text(s.seal(last)), want, "seal {name}");
    }
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    s.check_meta().unwrap();
}
