//! Upgrade-aware opens (D-WWAK7: automatic upgrades on first open). `AutoUpgrade` plans,
//! locks, plans again and applies the upgrade, then the engine completes it: its full fsck,
//! then the `FORMAT` mirror. A restart after the sealed batch resumes the completion with no
//! second backup or batch. `RequireCurrent` refuses a store that needs either, before anything
//! is written. These tests use an injected step registry; production registers no step.
use rdy_engine::{
    CompletionPhase, E_STORE_FORMAT, E_UPGRADE_FAILED, Local, Rdy, RdyError, Session, SessionKind,
    WriteRequest, with_completion_hook_for_tests,
};
use rdy_model::signature::Signature;
use rdy_model::{Hash, Provenance, Role, Tag};
use rdy_store::format::{
    FORMAT_FILE, FeatureClass, StoreFeature, StoreFeatures, TEST_FEATURE, baseline_features,
    inject_test_feature,
};
use rdy_store::holder::StoreMode;
use rdy_store::upgrade::{self, UpgradeStep};
use rdy_store::{OpenPolicy, Store, StoreError, kinds};
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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

/// The test feature this rdy registers while a test holds the guard: incompatible, as the
/// features a real upgrade adds are. No write here first uses it (its trigger is an intent).
fn known() -> rdy_store::format::TestFeature {
    inject_test_feature(FeatureClass::Incompatible, kinds::INTENT)
}

fn test_feature() -> StoreFeature {
    StoreFeature {
        name: TEST_FEATURE.into(),
        class: FeatureClass::Incompatible,
        required_rdy: "99.0.0".into(),
    }
}

/// One step: from any manifest to one with the test feature.
fn registry() -> Vec<UpgradeStep> {
    vec![UpgradeStep {
        id: "add_test".into(),
        from: StoreFeatures::of(vec![]),
        to: StoreFeatures::of(vec![test_feature()]),
    }]
}

fn upgraded() -> StoreFeatures {
    baseline_features().merged(&[test_feature()])
}

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write_doc(r: &mut Rdy, view: &str, title: &str) {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "page"}}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
}

/// A closed project with one landed doc; returns its base, root and the landing.
fn project() -> (tempfile::TempDir, PathBuf, Hash) {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    std::fs::create_dir(&root).unwrap();
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    write_doc(&mut r, "work", "Doc");
    let p = r
        .propose(&session(Role::Writer), "work", "Doc", Some("a summary"))
        .unwrap();
    let landing = r
        .land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap();
    r.close().unwrap();
    (base, root, landing)
}

fn store_dir(root: &Path) -> PathBuf {
    root.join(".rdy").join("store")
}

fn format_text(root: &Path) -> String {
    std::fs::read_to_string(store_dir(root).join(FORMAT_FILE)).unwrap()
}

/// The `meta/upgrade` frames in the store's raw segments: one per upgrade batch.
fn upgrade_batches(root: &Path) -> usize {
    let mut n = 0;
    for e in std::fs::read_dir(store_dir(root).join("segments")).unwrap() {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        n += bytes
            .windows(b"meta/upgrade".len())
            .filter(|w| *w == b"meta/upgrade")
            .count();
    }
    n
}

/// The upgrade backups beside the store.
fn backups(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join(".rdy"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(upgrade::BACKUP_PREFIX))
        .collect();
    out.sort();
    out
}

fn open_auto(root: &Path) -> Result<Rdy, RdyError> {
    Rdy::open_with_registry_for_tests(
        root,
        StoreMode::Project,
        OpenPolicy::AutoUpgrade,
        &registry(),
    )
}

/// Runs `f` with the completion hook recording each phase and failing at `fail`.
fn phases<T>(fail: Option<CompletionPhase>, f: impl FnOnce() -> T) -> (Vec<CompletionPhase>, T) {
    let seen = Rc::new(RefCell::new(vec![]));
    let rec = Rc::clone(&seen);
    let out = with_completion_hook_for_tests(
        move |p| {
            rec.borrow_mut().push(p);
            if Some(p) == fail {
                return Err(RdyError::new(rdy_engine::E_IO, "stopped here", "retry"));
            }
            Ok(())
        },
        f,
    );
    let seen = seen.borrow().clone();
    (seen, out)
}

/// Leaves the project's upgrade sealed and its completion pending: the upgrade stops after its
/// store check, before the engine ever runs.
fn make_pending(root: &Path) {
    let dir = store_dir(root);
    let stop = upgrade::with_hook_for_tests(
        |p| match p {
            upgrade::Phase::Checked => Err(std::io::Error::other("stopped after the check")),
            _ => Ok(()),
        },
        || Store::open_with_registry_for_tests(&dir, OpenPolicy::AutoUpgrade, &registry()),
    );
    assert!(stop.is_err());
    let plan = upgrade::inspect(&dir).unwrap();
    assert!(plan.pending_check && plan.steps.is_empty(), "{plan:?}");
    assert_eq!(upgrade_batches(root), 1);
}

fn assert_needs_upgrade(e: &RdyError) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_STORE_FORMAT,
            "the store needs an upgrade before it can open",
            "run the host's store maintenance, or rdy upgrade"
        ),
        "{e:?}"
    );
}

#[test]
fn ok9_require_current_never_upgrades() {
    let _f = known();
    let (base, root, _) = project();
    let before = tree(base.path());
    // planned steps: refused before any backup, append or mirror
    let e = Rdy::open_with_registry_for_tests(
        &root,
        StoreMode::Project,
        OpenPolicy::RequireCurrent,
        &registry(),
    )
    .err()
    .expect("steps are planned");
    assert_needs_upgrade(&e);
    let e = Store::open_with_registry_for_tests(
        &store_dir(&root),
        OpenPolicy::RequireCurrent,
        &registry(),
    )
    .err()
    .unwrap();
    assert!(matches!(e, StoreError::UpgradeRequired), "{e:?}");
    assert_eq!(tree(base.path()), before, "nothing was written");
    // a pending completion: refused too, with this rdy's (empty) registry
    make_pending(&root);
    let before = tree(base.path());
    assert_needs_upgrade(
        &Rdy::open_with_policy(&root, OpenPolicy::RequireCurrent)
            .err()
            .expect("the completion is pending"),
    );
    assert_needs_upgrade(
        &Local::open_with_policy(&root, OpenPolicy::RequireCurrent)
            .err()
            .expect("the completion is pending"),
    );
    assert!(matches!(
        Store::open_with_policy(&store_dir(&root), OpenPolicy::RequireCurrent),
        Err(StoreError::UpgradeRequired)
    ));
    assert_eq!(tree(base.path()), before, "nothing was written");
    // the default policy completes it: fsck, then the mirror, with no second batch
    let r = Rdy::open(&root).unwrap();
    assert!(!r.store().upgrade_pending());
    assert_eq!(r.store().features(), &upgraded());
    r.close().unwrap();
    assert_eq!(format_text(&root), upgraded().to_text());
    assert_eq!(upgrade_batches(&root), 1);
    // now current: RequireCurrent opens it
    Rdy::open_with_policy(&root, OpenPolicy::RequireCurrent)
        .unwrap()
        .close()
        .unwrap();
}

#[test]
fn ok9_auto_open_resumes_the_one_upgrade_batch() {
    let _f = known();
    let (_base, root, _) = project();
    let old = format_text(&root);
    // a stop before the full fsck, then one after it and before the mirror's publication
    for stop in [CompletionPhase::BeforeFsck, CompletionPhase::Checked] {
        let (seen, out) = phases(Some(stop), || open_auto(&root));
        let e = out.err().expect("the completion stopped");
        assert_eq!(e.message, "stopped here");
        let want = match stop {
            CompletionPhase::BeforeFsck => vec![CompletionPhase::BeforeFsck],
            _ => vec![CompletionPhase::BeforeFsck, CompletionPhase::Checked],
        };
        assert_eq!(seen, want);
        assert_eq!(upgrade_batches(&root), 1, "{stop:?}: one sealed batch");
        assert_eq!(backups(&root).len(), 1, "{stop:?}: one backup");
        assert_eq!(format_text(&root), old, "{stop:?}: the old mirror");
        let plan = upgrade::inspect(&store_dir(&root)).unwrap();
        assert!(plan.pending_check && plan.steps.is_empty(), "{plan:?}");
    }
    // the next open resumes: fsck, then the mirror; no second backup or batch
    let (seen, out) = phases(None, || open_auto(&root));
    let mut r = out.unwrap();
    assert_eq!(
        seen,
        vec![
            CompletionPhase::BeforeFsck,
            CompletionPhase::Checked,
            CompletionPhase::Published
        ]
    );
    assert!(!r.store().upgrade_pending());
    assert_eq!(r.store().features(), &upgraded());
    assert_eq!(format_text(&root), upgraded().to_text());
    assert_eq!(upgrade_batches(&root), 1);
    assert_eq!(backups(&root).len(), 1);
    // the completed store writes again
    write_doc(&mut r, "more", "Another doc");
    r.close().unwrap();
    // a later open has nothing to complete
    let (seen, out) = phases(None, || open_auto(&root));
    out.unwrap().close().unwrap();
    assert!(seen.is_empty(), "{seen:?}");
    assert_eq!(upgrade_batches(&root), 1);
    // a fresh project upgrades and completes in its first open
    let (_base, fresh, _) = project();
    let (seen, out) = phases(None, || open_auto(&fresh));
    out.unwrap().close().unwrap();
    assert_eq!(seen.len(), 3);
    assert_eq!(format_text(&fresh), upgraded().to_text());
    assert_eq!(upgrade_batches(&fresh), 1);
}

fn bad_tag(landing: Hash) -> Tag {
    Tag {
        name: "release-1".into(),
        landing,
        message: None,
        tagged_by: Provenance {
            actor: "x".into(),
            run: None,
            role: Role::Approver,
            at_ms: 0,
        },
    }
}

fn bad_signature(landing: Hash) -> Signature {
    Signature {
        subject: landing,
        namespace: "rdy-approval".into(),
        statement: b"not a statement".to_vec(),
        sig: b"damaged".to_vec(),
        signed_by: Provenance {
            actor: "human:founder".into(),
            run: None,
            role: Role::Approver,
            at_ms: 1,
        },
    }
}

/// A fold that breaks: a tension resolves while it is not open.
fn bad_fold() -> (u8, Vec<u8>) {
    use rdy_store::transition::*;
    let side = |c: &[u8], actor: &str| Side {
        change: Some(Hash::of(c)),
        actor: Some(actor.into()),
        run: Some("r".into()),
        landing: None,
    };
    Transitions {
        at_ms: 1,
        resolved: vec![TensionMove {
            tension: Hash::of(b"t1"),
            kind: "edit_collision".into(),
            severity: "blocking".into(),
            sides: vec![side(b"c1", "agent:a"), side(b"c2", "agent:b")],
            rekey: None,
            why: Why {
                cause: Cause::Committed,
                id: Some(Hash::of(b"c1")),
                actor: None,
                run: None,
            },
        }],
        opened: vec![],
        gone: vec![],
        new: vec![],
    }
    .frame()
}

#[test]
fn ok9_completion_refuses_corrupt_tags_signatures_and_folds() {
    let _f = known();
    for case in ["tag", "signature", "fold"] {
        let (_base, root, landing) = project();
        {
            let mut r = Rdy::open(&root).unwrap();
            match case {
                "tag" => {
                    r.store_mut().put_tag(&bad_tag(landing)).unwrap();
                }
                "signature" => {
                    r.store_mut()
                        .put_signature(&bad_signature(landing))
                        .unwrap();
                }
                _ => r.store_mut().seal(vec![bad_fold()]).unwrap(),
            }
            r.seal_open().unwrap();
            assert!(r.fsck().is_err(), "{case}: fsck fails");
            r.close().unwrap();
        }
        let old = format_text(&root);
        for attempt in 0..2 {
            let e = open_auto(&root).err().expect("the completion fails");
            let names = backups(&root);
            assert_eq!(names.len(), 1, "{case}: one backup");
            assert_eq!(
                (e.code, e.message.clone()),
                (
                    E_UPGRADE_FAILED,
                    format!(
                        "the upgrade batch is durable, but the store check failed; backup: {}",
                        names[0]
                    )
                ),
                "{case} {attempt}"
            );
            assert_eq!(
                e.fix,
                "stop the daemon and restore the backup before retrying"
            );
            assert_eq!(format_text(&root), old, "{case}: the old mirror stays");
            assert_eq!(upgrade_batches(&root), 1, "{case}: one batch");
        }
    }
}

/// A holder's completion runs the holder's fsck (`Rdy::fsck` dispatches to it): its tag is
/// checked against its own history. A project's fsck would refuse the tag, since a holder has
/// no canon chain for it to be in.
#[test]
fn ok9_holder_completion_runs_the_holder_fsck() {
    let _f = known();
    let (base, root, _) = project();
    let mut r = Rdy::open(&root).unwrap();
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    // the tag's history, as a holder takes it: every copied object, then the tag's ref
    let copied = [
        kinds::BLOCK,
        kinds::CHANGE,
        kinds::PROPOSAL,
        kinds::LANDING,
        kinds::SNAPSHOT,
        kinds::BLOB_CHUNK,
        kinds::BLOB_MANIFEST,
        kinds::TAG,
    ];
    let mut items = vec![];
    r.store()
        .for_each_frame_from(
            rdy_store::segment::Location {
                segment: 0,
                offset: 0,
            },
            r.store().log_end(),
            |_, kind, _, payload, _| {
                if copied.contains(&rdy_store::codec::logical_kind(kind)) {
                    items.push((kind, payload.to_vec()));
                }
                Ok(())
            },
        )
        .unwrap();
    let refs: Vec<(String, Vec<u8>)> = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .map(|(n, h, _)| (format!("tag/{n}"), h.0.to_vec()))
        .collect();
    r.close().unwrap();
    let holder = base.path().join("h");
    let mut h = Rdy::create_holder(&holder, rdy_model::RepoId([1; 16])).unwrap();
    h.store_mut().write(items).unwrap();
    h.store_mut().set_refs(refs).unwrap();
    h.seal_open().unwrap();
    h.close().unwrap();
    make_pending(&holder);
    let (seen, out) = phases(None, || {
        Rdy::open_with_registry_for_tests(
            &holder,
            StoreMode::Holder,
            OpenPolicy::AutoUpgrade,
            &registry(),
        )
    });
    let h = out.unwrap();
    assert_eq!(
        seen,
        vec![
            CompletionPhase::BeforeFsck,
            CompletionPhase::Checked,
            CompletionPhase::Published
        ]
    );
    assert!(!h.store().upgrade_pending());
    assert_eq!(
        h.fsck().unwrap().versions,
        1,
        "the holder's fsck checks its tag"
    );
    h.close().unwrap();
    assert_eq!(format_text(&holder), upgraded().to_text());
    assert_eq!(upgrade_batches(&holder), 1);
}
