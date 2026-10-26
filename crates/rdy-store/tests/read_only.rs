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

/// Every path under a directory, with its length and contents (None for a directory).
type Tree = BTreeMap<PathBuf, Option<(u64, Vec<u8>)>>;

/// Every path under `dir` with its length and contents (None for a directory).
fn tree(dir: &Path) -> Tree {
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
    // a holder's take of upload batches: refused before a file is read or its check runs
    let upload = tempfile::NamedTempFile::new().unwrap();
    let target = rdy_store::take_batch::UploadTarget {
        tag: "v1.0.0".into(),
        landing: Hash::of(b"a landing"),
    };
    let mut checked = false;
    match s.take_batches(
        &[std::fs::File::open(upload.path()).unwrap()],
        &target,
        |_, _| {
            checked = true;
            Err::<rdy_store::take_batch::TagChoice, ()>(())
        },
    ) {
        Err(rdy_store::take_batch::TakeError::Store(e)) => read_only_error(e),
        Err(_) => panic!("not the read-only refusal"),
        Ok(_) => panic!("a read-only store took a batch"),
    }
    assert!(!checked, "the take's check never ran");
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
    // the last acknowledged batch, damaged where no later marker follows: the index's floor
    // says it was acknowledged, so the read-only open refuses it exactly as the writable open
    // does, and no read (and so no dump) is served without it
    last_batch_damage_is_refused();
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

/// The store with `t/a`, the blob that activated the read-only test feature and `t/z` as the
/// last batch, closed cleanly (the index's floor is the log's end), and one byte flipped inside
/// the first frame of that last batch: its marker is intact. Returns the base, the store and
/// where the last batch starts.
fn damaged_last_batch() -> (tempfile::TempDir, PathBuf, rdy_store::segment::Location) {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let start = {
        let _known = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::BLOB_CHUNK);
        let mut s = Store::create(&dir).unwrap();
        s.set_refs(vec![("t/a".into(), vec![1])]).unwrap();
        s.put_blob(BLOB).unwrap();
        let start = s.log_end();
        s.set_refs(vec![("t/z".into(), vec![26; 64])]).unwrap();
        assert!(
            s.log_end().offset > start.offset + 37 + 8,
            "one whole batch"
        );
        s.close().unwrap();
        start
    };
    let seg = dir
        .join("segments")
        .join(format!("seg-{:06}.log", start.segment));
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[start.offset as usize + 37 + 4] ^= 0xff;
    std::fs::write(&seg, &bytes).unwrap();
    (base, dir, start)
}

fn last_batch_damage_is_refused() {
    let (_base, dir, start) = damaged_last_batch();
    let parent = dir.parent().unwrap().to_path_buf();
    let before = tree(&parent);
    let refusal = |e: StoreError| {
        assert!(
            matches!(&e, StoreError::Io(io) if io.kind() == std::io::ErrorKind::InvalidData),
            "{e:?}"
        );
        let text = e.to_string();
        assert!(
            text.contains(&format!(
                "corruption at offset {} in {}",
                start.offset,
                dir.join("segments").join("seg-000000.log").display()
            )) && text.contains("damages acknowledged data; refusing to truncate"),
            "{text}"
        );
        text
    };
    let mut texts = vec![];
    for policy in [OpenPolicy::AutoUpgrade, OpenPolicy::RequireCurrent] {
        let e = Store::open_with_policy(&dir, policy)
            .err()
            .expect("the damaged last batch is refused read-only");
        texts.push(refusal(e));
    }
    assert_eq!(
        tree(&parent),
        before,
        "the read-only refusal changed nothing"
    );
    // the writable open, with the feature known, refuses the same damage with the same text
    let _known = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::BLOB_CHUNK);
    let e = Store::open(&dir)
        .err()
        .expect("the damaged last batch is refused writable");
    assert_eq!(
        refusal(e),
        texts[0],
        "both opens decide the tail the same way"
    );
}

#[test]
fn ok9_unknown_encoding_refuses_the_open() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let version = rdy_store::codec::latest(kinds::TAG) + 1;
    {
        let _known = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::BLOB_CHUNK);
        let _newer = rdy_store::codec::test_encoding(kinds::TAG, version);
        let mut s = Store::create(&dir).unwrap();
        s.put_blob(BLOB).unwrap();
        let tag = Tag {
            name: "v1.0.0".into(),
            landing: Hash::of(b"a landing"),
            message: None,
            tagged_by: Provenance {
                actor: "human:someone".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1,
            },
        };
        s.write(vec![rdy_store::codec::item_new(kinds::TAG, &tag)])
            .unwrap();
        s.close().unwrap();
    }
    let parent = dir.parent().unwrap().to_path_buf();
    let before = tree(&parent);
    for policy in [OpenPolicy::AutoUpgrade, OpenPolicy::RequireCurrent] {
        let e = Store::open_with_policy(&dir, policy)
            .err()
            .expect("an object this rdy cannot read refuses the open");
        match e {
            StoreError::Format { message, fix } => assert_eq!(
                (message.as_str(), fix.as_str()),
                (
                    format!(
                        "the store holds an object this rdy cannot read: kind TAG, encoding {version}"
                    )
                    .as_str(),
                    "open the project with a newer rdy"
                ),
                "{policy:?}"
            ),
            other => panic!("{policy:?}: not the format refusal: {other:?}"),
        }
    }
    assert_eq!(tree(&parent), before, "no source byte changed");
}

#[test]
fn ok9_pending_upgrade_refuses_a_read_only_open() {
    use rdy_store::format::{StoreFeature, StoreFeatures};
    use rdy_store::upgrade::{self, UpgradeStep};
    let f = read_only_store();
    // a newer rdy, which knows the read-only feature, seals an upgrade that adds a feature
    // this rdy does not know either, and stops before its completion publishes FORMAT
    let added = StoreFeature {
        name: "zz_added".into(),
        class: FeatureClass::Compatible,
        required_rdy: "100.2.0".into(),
    };
    let registry = vec![UpgradeStep {
        id: "add_zz".into(),
        from: StoreFeatures::of(vec![]),
        to: StoreFeatures::of(vec![added]),
    }];
    {
        let _known = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::BLOB_CHUNK);
        let lock = StoreLock::take(&f.dir).unwrap();
        let plan = upgrade::inspect_with_registry_for_tests(&f.dir, &registry).unwrap();
        assert_eq!(plan.steps, ["add_zz"]);
        upgrade::apply_locked_with_registry_for_tests(&f.dir, &lock, &plan, &registry).unwrap();
    }
    let format_before = std::fs::read(f.dir.join("FEATURES")).unwrap();
    let parent = f.dir.parent().unwrap().to_path_buf();
    let before = tree(&parent);
    for policy in [OpenPolicy::AutoUpgrade, OpenPolicy::RequireCurrent] {
        let e = Store::open_with_policy(&f.dir, policy)
            .err()
            .expect("a pending upgrade is never read");
        match e {
            StoreError::Format { message, fix } => assert_eq!(
                (message.as_str(), fix.as_str()),
                (
                    "the store has an upgrade a newer rdy started and did not finish",
                    "open the project with rdy 100.2.0 or later to finish the upgrade"
                ),
                "{policy:?}"
            ),
            other => panic!("{policy:?}: not the format refusal: {other:?}"),
        }
        assert_eq!(tree(&parent), before, "{policy:?}: no source byte changed");
    }
    assert_eq!(
        std::fs::read(f.dir.join("FEATURES")).unwrap(),
        format_before
    );
}

/// A newer rdy sealed an upgrade that adds a read-only feature this rdy does not know, on a
/// store whose `FORMAT` this rdy may write, and stopped before its completion published `FORMAT`.
/// Every open refuses it with the unfinished-upgrade text (ruling task-9), never the older
/// "needs rdy" text, before anything is written.
#[test]
fn ok9_a_pending_upgrade_to_an_unknown_read_only_feature_is_unfinished() {
    use rdy_store::format::{StoreFeature, StoreFeatures, TEST_FEATURE, TEST_FEATURE_RDY};
    use rdy_store::upgrade::{self, UpgradeStep};
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("store");
    Store::create(&dir).unwrap().close().unwrap();
    let registry = vec![UpgradeStep {
        id: "add_read_only".into(),
        from: StoreFeatures::of(vec![]),
        to: StoreFeatures::of(vec![StoreFeature {
            name: TEST_FEATURE.into(),
            class: FeatureClass::ReadOnlyCompatible,
            required_rdy: TEST_FEATURE_RDY.into(),
        }]),
    }];
    {
        let _known = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::BLOB_CHUNK);
        let lock = StoreLock::take(&dir).unwrap();
        let plan = upgrade::inspect_with_registry_for_tests(&dir, &registry).unwrap();
        assert_eq!(plan.steps, ["add_read_only"]);
        upgrade::apply_locked_with_registry_for_tests(&dir, &lock, &plan, &registry).unwrap();
    }
    let before = tree(base.path());
    for policy in [OpenPolicy::AutoUpgrade, OpenPolicy::RequireCurrent] {
        match Store::open_with_policy(&dir, policy).err() {
            Some(StoreError::Format { message, fix }) => assert_eq!(
                (message.as_str(), fix.as_str()),
                (
                    "the store has an upgrade a newer rdy started and did not finish",
                    format!("open the project with rdy {TEST_FEATURE_RDY} or later to finish the upgrade")
                        .as_str()
                ),
                "{policy:?}"
            ),
            other => panic!("{policy:?}: not the unfinished-upgrade refusal: {other:?}"),
        }
        assert_eq!(tree(base.path()), before, "{policy:?}: no byte changed");
    }
}

/// A store whose upgrade plan cannot be made never opens as current: the planner's refusal is
/// the open's, under both policies, and nothing is upgraded or written.
#[cfg(unix)]
#[test]
fn ok9_a_plan_that_cannot_be_made_never_opens() {
    use rdy_store::error::UpgradeFailure;
    use rdy_store::format::{StoreFeature, StoreFeatures, TEST_FEATURE};
    use rdy_store::upgrade::UpgradeStep;
    let feature = |name: &str, class| StoreFeature {
        name: name.into(),
        class,
        required_rdy: "99.0.0".into(),
    };
    let add_test = vec![UpgradeStep {
        id: "add_test".into(),
        from: StoreFeatures::of(vec![]),
        to: StoreFeatures::of(vec![feature(TEST_FEATURE, FeatureClass::Incompatible)]),
    }];
    let _known = inject_test_feature(FeatureClass::Incompatible, kinds::INTENT);
    // a stray link in the store: the planner refuses its backup; nothing opens or changes
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    s.set_refs(vec![("t/a".into(), vec![1])]).unwrap();
    s.close().unwrap();
    rdy_store::pio::symlink(dir.join("FORMAT"), dir.join("stray")).unwrap();
    let before = tree(base.path());
    for policy in [OpenPolicy::AutoUpgrade, OpenPolicy::RequireCurrent] {
        let e = Store::open_with_registry_for_tests(&dir, policy, &add_test)
            .err()
            .expect("a plan that cannot be made never opens");
        assert!(
            matches!(
                e,
                StoreError::Upgrade {
                    failure: UpgradeFailure::BackupFileType,
                    backup: None
                }
            ),
            "{policy:?}: {e:?}"
        );
        assert_eq!(tree(base.path()), before, "{policy:?}");
    }
    // a planned manifest with a feature this rdy cannot write: the planner's format refusal,
    // though the store itself opens
    std::fs::remove_file(dir.join("stray")).unwrap();
    let unknown = vec![UpgradeStep {
        id: "add_unknown".into(),
        from: StoreFeatures::of(vec![]),
        to: StoreFeatures::of(vec![feature("zz_unknown", FeatureClass::Incompatible)]),
    }];
    for policy in [OpenPolicy::AutoUpgrade, OpenPolicy::RequireCurrent] {
        let e = Store::open_with_registry_for_tests(&dir, policy, &unknown)
            .err()
            .expect("a plan that cannot be made never opens");
        assert!(
            matches!(&e, StoreError::Format { message, .. } if message == "store feature zz_unknown needs rdy 99.0.0"),
            "{policy:?}: {e:?}"
        );
    }
    let s = Store::open(&dir).unwrap();
    assert!(!s.features().has(TEST_FEATURE), "nothing was upgraded");
    assert_eq!(s.get_ref("t/a").unwrap(), Some(vec![1]));
}
