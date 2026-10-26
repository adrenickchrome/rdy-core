//! Read-only stores (D-WWAK7: "It opens a store with an unknown read-only feature for reading
//! only."). The open reads the sealed log into a private index and changes no byte of the
//! source store: no sentinel, quarantine, cut, `FORMAT` repair, source index or removal. Every
//! write entry refuses before it appends, and every reader holds the store's lock and the
//! private index until the last one drops.
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_store::format::{FeatureClass, StoreAccess, inject_test_feature};
use rdy_store::inbox::{InboxCursor, InboxSub};
use rdy_store::{OpenPolicy, Store, StoreError, StoreLock, kinds};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every path under `dir` with its length and contents (None for a directory).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<(u64, Vec<u8>)>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let m = std::fs::symlink_metadata(&p).unwrap();
        if m.is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some((m.len(), std::fs::read(&p).unwrap())));
        }
    }
    out
}

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
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
    }
}

const BLOB: &[u8] = b"uses the read-only feature";

/// A store with a view, a block and a blob whose first chunk activated the read-only test
/// feature. Once the guard drops, this rdy no longer knows the feature: the store is
/// read-only. Returns the base, the store path, the block and the blob.
struct Fixture {
    _base: tempfile::TempDir,
    dir: PathBuf,
    block: Block,
    blob: Hash,
}

fn read_only_store() -> Fixture {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let b = block("b-00001", "a body that stays readable");
    let blob = {
        let _known = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::BLOB_CHUNK);
        let mut s = Store::create(&dir).unwrap();
        s.commit_to_view(
            "work",
            change("work", vec![Op::Create { block: b.clone() }]),
        )
        .unwrap();
        s.set_refs(vec![("t/a".into(), vec![1])]).unwrap();
        let blob = s.put_blob(BLOB).unwrap();
        s.close().unwrap();
        blob
    };
    Fixture {
        _base: base,
        dir,
        block: b,
        blob,
    }
}

fn read_only_error(e: StoreError) {
    match e {
        StoreError::ReadOnly {
            feature,
            required_rdy,
        } => {
            assert_eq!(
                (feature.as_str(), required_rdy.as_str()),
                ("test_feature", "99.0.0")
            );
        }
        other => panic!("not a read-only refusal: {other:?}"),
    }
}

/// The reads every test here makes: the block, the blob, a ref and the whole-log checks.
fn reads(s: &rdy_store::StoreReader, f: &Fixture) {
    assert_eq!(s.get_block(&f.block.version_hash()).unwrap(), f.block);
    assert_eq!(s.get_blob(&f.blob).unwrap(), BLOB);
    assert_eq!(s.get_ref("t/a").unwrap(), Some(vec![1]));
    assert!(s.check_objects().is_ok());
}

#[test]
fn ok9_read_only_open_changes_no_source_byte() {
    for with_index in [true, false] {
        let f = read_only_store();
        if !with_index {
            std::fs::remove_file(f.dir.join("index.redb")).unwrap();
        }
        let parent = f.dir.parent().unwrap().to_path_buf();
        let before = tree(&parent);
        for policy in [OpenPolicy::AutoUpgrade, OpenPolicy::RequireCurrent] {
            let s = Store::open_with_policy(&f.dir, policy).unwrap();
            assert_eq!(s.access(), StoreAccess::ReadOnly, "{with_index} {policy:?}");
            assert_eq!(s.reader().access(), StoreAccess::ReadOnly);
            assert!(!s.upgrade_pending());
            reads(&s, &f);
            assert!(s.verify().unwrap() > 0);
            s.verify_report().unwrap();
            assert_eq!(s.open_report().quarantined, Vec::<PathBuf>::new());
            assert!(!s.open_report().unclean_shutdown, "{with_index}");
            let private = s.reader().private_index_dir_for_tests().unwrap();
            assert!(
                !private.starts_with(&parent),
                "the private index is outside the source"
            );
            s.close().unwrap();
            assert!(
                !private.exists(),
                "the last reader removed the private index"
            );
            assert_eq!(
                tree(&parent),
                before,
                "{with_index} {policy:?}: no path, byte or length changed"
            );
        }
        // the same open under a lock the caller took on the existing lock file
        let lock = StoreLock::take_existing(&f.dir).unwrap();
        let s = Store::open_locked(&f.dir, &lock).unwrap();
        reads(&s, &f);
        drop(s);
        drop(lock);
        assert_eq!(
            tree(&parent),
            before,
            "{with_index}: under the caller's lock"
        );
        // a store with no lock file is refused, and no lock file is made
        std::fs::remove_file(f.dir.join("lock")).unwrap();
        let before = tree(&parent);
        let e = Store::open(&f.dir).err().expect("no lock file");
        assert!(matches!(e, StoreError::ReadOnlyNoLock), "{e:?}");
        assert_eq!(e.to_string(), "the read-only store has no lock file");
        assert!(matches!(
            StoreLock::take_existing(&f.dir),
            Err(StoreError::ReadOnlyNoLock)
        ));
        assert_eq!(
            tree(&parent),
            before,
            "{with_index}: no lock file was created"
        );
    }
}

#[test]
fn ok9_every_mutation_entry_refuses_before_append() {
    let f = read_only_store();
    let before = tree(&f.dir);
    let mut s = Store::open(&f.dir).unwrap();
    let end = s.log_end();
    let features = s.features().clone();
    // append: an object, a blob, a raw write and a change
    read_only_error(s.put_blob(b"a new blob").unwrap_err());
    read_only_error(
        s.write(vec![(kinds::BLOB_CHUNK, b"raw".to_vec())])
            .unwrap_err(),
    );
    read_only_error(
        s.commit_to_view(
            "work",
            change(
                "work",
                vec![Op::Create {
                    block: block("b-00002", "new"),
                }],
            ),
        )
        .unwrap_err(),
    );
    // ref: a ref and an inbox subscription
    read_only_error(s.set_refs(vec![("t/b".into(), vec![2])]).unwrap_err());
    let sub = InboxSub {
        actor: "agent:a".into(),
        run: None,
        cursor: InboxCursor { at: end, skip: 0 },
        areas: vec![],
        role: "writer".into(),
    };
    read_only_error(s.put_inbox(&sub).unwrap_err());
    // seal: a held batch opens nothing, and its seal of a last frame refuses
    s.set_held(true).unwrap();
    read_only_error(s.set_refs(vec![("t/b".into(), vec![2])]).unwrap_err());
    assert!(s.open_batch().is_none(), "no held batch opened");
    read_only_error(
        s.seal(vec![(kinds::BLOB_CHUNK, b"last".to_vec())])
            .unwrap_err(),
    );
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    // removal: the apply writer and the finishing of removals
    read_only_error(s.seal_redaction_batch_for_tests(vec![]).unwrap_err());
    read_only_error(s.finish_redactions().unwrap_err());
    // the source index: a rebuild
    read_only_error(s.reindex().unwrap_err());
    read_only_error(s.ensure_writable().unwrap_err());
    read_only_error(s.reader().ensure_writable().unwrap_err());
    // nothing moved: the log, the batch, the features; a checkpoint is of the private index
    assert_eq!(s.log_end(), end);
    assert!(s.open_batch().is_none());
    assert_eq!(s.features(), &features);
    s.checkpoint().unwrap();
    reads(&s, &f);
    s.close().unwrap();
    assert_eq!(tree(&f.dir), before, "no source byte changed");
    // an rdy that knows the feature writes the same store
    let _known = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::BLOB_CHUNK);
    let mut s = Store::open(&f.dir).unwrap();
    assert_eq!(s.access(), StoreAccess::ReadWrite);
    s.put_blob(b"a new blob").unwrap();
}

#[test]
fn ok9_read_only_reads_use_one_snapshot() {
    let f = read_only_store();
    let s = Store::open(&f.dir).unwrap();
    let r1 = s.reader();
    let end = s.log_end();
    s.close().unwrap();
    // a reader outlives its store: it reads the same snapshot and keeps the lock
    reads(&r1, &f);
    let r2 = r1.clone();
    let private = r1.private_index_dir_for_tests().unwrap();
    assert!(matches!(Store::open(&f.dir), Err(StoreError::Locked(_))));
    drop(r1);
    assert!(private.exists(), "a reader is left");
    reads(&r2, &f);
    drop(r2);
    assert!(
        !private.exists(),
        "the last reader removed the private index"
    );
    let s = Store::open(&f.dir).unwrap();
    assert_eq!(s.log_end(), end);
    reads(&s, &f);
}

/// Appends `bytes` to the store's last segment.
fn append_raw(dir: &Path, bytes: &[u8]) {
    use std::io::Write;
    let seg = dir.join("segments").join("seg-000000.log");
    let mut file = std::fs::OpenOptions::new().append(true).open(seg).unwrap();
    file.write_all(bytes).unwrap();
}

/// One whole frame of `kind` and `payload`, with no batch marker.
fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_le_bytes().to_vec();
    out.push(kind);
    out.extend_from_slice(&Hash::of(payload).0);
    out.extend_from_slice(payload);
    out
}

#[test]
fn ok9_read_only_tail_and_pending_removal_fail_closed() {
    // a torn tail and an unmarked batch: ignored, reported, never cut or copied
    for (what, tail) in [
        (
            "torn",
            frame(kinds::BLOB_CHUNK, b"a torn frame")[..20].to_vec(),
        ),
        (
            "unmarked",
            frame(kinds::BLOB_CHUNK, b"a whole frame, no marker"),
        ),
    ] {
        let f = read_only_store();
        let segs = f.dir.join("segments");
        assert!(segs.join("seg-000000.log").exists(), "one segment");
        append_raw(&f.dir, &tail);
        let parent = f.dir.parent().unwrap().to_path_buf();
        let before = tree(&parent);
        let s = Store::open(&f.dir).unwrap();
        assert_eq!(s.access(), StoreAccess::ReadOnly);
        assert!(s.open_report().unclean_shutdown, "{what}: reported");
        assert!(s.open_report().quarantined.is_empty(), "{what}");
        reads(&s, &f);
        drop(s);
        assert_eq!(tree(&parent), before, "{what}: the tail is still there");
    }
    // a damaged acknowledged frame: the existing corruption refusal, nothing changed
    let f = read_only_store();
    let seg = f.dir.join("segments").join("seg-000000.log");
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[37 + 2] ^= 0xff; // inside the first frame's payload, before every marker
    std::fs::write(&seg, &bytes).unwrap();
    let before = tree(&f.dir);
    let e = Store::open(&f.dir).err().expect("damage is refused");
    assert!(
        matches!(&e, StoreError::Io(io) if io.kind() == std::io::ErrorKind::InvalidData),
        "{e:?}"
    );
    assert!(e.to_string().contains("damages acknowledged data"), "{e}");
    assert_eq!(tree(&f.dir), before, "nothing was quarantined or cut");
    // an authorized removal that has not finished: E_REDACTION_PENDING before a read escapes
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let secret = block("b-00001", "canary alpha secret");
    {
        let _known = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::BLOB_CHUNK);
        let mut s = Store::create(&dir).unwrap();
        s.commit_to_view(
            "work",
            change(
                "work",
                vec![
                    Op::Create {
                        block: secret.clone(),
                    },
                    Op::Create {
                        block: block("b-00002", "bravo stays"),
                    },
                ],
            ),
        )
        .unwrap();
        s.put_blob(BLOB).unwrap();
        s.close().unwrap();
        let spec = RequestSpec::new(
            "work",
            vec![RedactTarget::Version {
                id: secret.id.clone(),
                version: secret.version_hash(),
            }],
        );
        authorized_batch_fixture(
            &dir,
            &[spec],
            &FixtureOptions::approved(Proof::local_human()),
        );
    }
    let before = tree(&dir);
    let e = Store::open(&dir).err().expect("the removal is unfinished");
    assert!(matches!(e, StoreError::RedactionPending), "{e:?}");
    assert_eq!(
        e.to_string(),
        "Content removal was authorized but has not finished."
    );
    assert_eq!(tree(&dir), before, "the removal was not finished here");
}
