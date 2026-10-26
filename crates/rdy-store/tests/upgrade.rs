#![cfg(unix)]
// Unix only: it sets file modes, makes symlinks and reads a child's signal. FW-XP1 Phase 4 ports it.
//! The store upgrade transaction (D-WWAK7): a dry run writes nothing; an upgrade backs the closed
//! store up completely, then appends one marked batch with the new manifest and its receipt,
//! rebuilds the index and runs the store check. FORMAT keeps the old mirror until the engine's
//! completion publishes it. Every failure keeps the acknowledged bytes and the backup. These
//! tests use an injected step registry; production registers no step.
use rdy_model::*;
use rdy_store::error::UpgradeFailure;
use rdy_store::format::{
    self, FORMAT_FILE, FeatureClass, REF_STORE_FEATURES, REF_UPGRADE, StoreFeature, StoreFeatures,
    TEST_FEATURE, UpgradeReceipt, baseline_features, inject_test_feature,
};
use rdy_store::upgrade::{
    self, MAX_BACKUP_ENTRIES, MAX_BACKUP_PATH_BYTES, Phase, UpgradePlan, UpgradeReport,
    UpgradeStep, apply_locked_with_registry_for_tests as apply_with,
    inspect_with_registry_for_tests as inspect_with, with_backup_limits_for_tests,
    with_hook_for_tests,
};
use rdy_store::{Store, StoreError, StoreLock, kinds};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};

// ---------------------------------------------------------------------------------------------
// fixtures

fn feature(name: &str, class: FeatureClass) -> StoreFeature {
    StoreFeature {
        name: name.into(),
        class,
        required_rdy: "99.0.0".into(),
    }
}

fn manifest(features: Vec<StoreFeature>) -> StoreFeatures {
    StoreFeatures::of(features)
}

/// The test feature this rdy registers while a test holds the guard: incompatible, as the
/// features a real upgrade adds are.
fn known() -> format::TestFeature {
    // the trigger kind is one no test here writes, so no write first uses it
    inject_test_feature(FeatureClass::Incompatible, kinds::INTENT)
}

fn test_feature() -> StoreFeature {
    feature(TEST_FEATURE, FeatureClass::Incompatible)
}

/// Three steps, listed out of order: `beta` and `zeta` start from any manifest, `alpha` needs
/// what `zeta` makes. Dependency order, then id: beta, zeta, alpha.
fn registry() -> Vec<UpgradeStep> {
    let compat = |n: &str| feature(n, FeatureClass::Compatible);
    vec![
        UpgradeStep {
            id: "alpha".into(),
            from: manifest(vec![test_feature()]),
            to: manifest(vec![test_feature(), compat("zz_extra")]),
        },
        UpgradeStep {
            id: "zeta".into(),
            from: manifest(vec![]),
            to: manifest(vec![test_feature()]),
        },
        UpgradeStep {
            id: "beta".into(),
            from: manifest(vec![]),
            to: manifest(vec![compat("aa_other")]),
        },
    ]
}

const STEPS: [&str; 3] = ["beta", "zeta", "alpha"];

/// The manifest the registry makes from the baseline (ok4: content-2 and object encodings).
fn upgraded() -> StoreFeatures {
    baseline_features().merged(&[
        feature("aa_other", FeatureClass::Compatible),
        test_feature(),
        feature("zz_extra", FeatureClass::Compatible),
    ])
}

fn block(id: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: "x".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: "human:someone".into(),
        run: None,
        role,
        at_ms: 1,
    }
}

fn create(view: &str, id: &str) -> Change {
    Change {
        view: view.into(),
        ops: vec![Op::Create { block: block(id) }],
        deps: vec![],
        provenance: prov(Role::Writer),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// A closed project at `<base>/.rdy/store` with views, a landing and a tag.
fn project(base: &Path) -> PathBuf {
    let dir = base.join(".rdy").join("store");
    std::fs::create_dir_all(base.join(".rdy")).unwrap();
    let mut s = Store::create(&dir).unwrap();
    for (v, id) in [("a", "b-00001"), ("a", "b-00002"), ("b", "b-00003")] {
        s.commit_to_view(v, create(v, id)).unwrap();
    }
    let p = s.propose("a", "a", prov(Role::Writer)).unwrap();
    let landing = s.land(&p, prov(Role::Approver)).unwrap();
    s.commit_to_view("a", create("a", "b-00004")).unwrap();
    s.put_tag(&Tag {
        name: "v1.0.0".into(),
        landing,
        message: Some("first".into()),
        tagged_by: prov(Role::Approver),
    })
    .unwrap();
    s.check_objects().unwrap();
    s.close().unwrap();
    dir
}

/// Every path under `dir` with its contents (None for a directory or a special file).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let m = std::fs::symlink_metadata(&p).unwrap();
        if m.is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else if m.is_file() {
            out.insert(p.clone(), Some(std::fs::read(&p).unwrap()));
        } else {
            out.insert(p.clone(), None);
        }
    }
    out
}

/// The store's segment files and their bytes.
fn segments(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir.join("segments")).unwrap() {
        let p = e.unwrap().path();
        if !p.file_name().unwrap().to_string_lossy().starts_with("seg-") {
            continue;
        }
        out.insert(
            p.file_name().unwrap().to_string_lossy().into_owned(),
            std::fs::read(&p).unwrap(),
        );
    }
    out
}

/// Every segment file of `before` is unchanged or only grew: each acknowledged byte is kept.
fn keeps_bytes(before: &BTreeMap<String, Vec<u8>>, after: &BTreeMap<String, Vec<u8>>) {
    for (name, old) in before {
        let new = after.get(name).unwrap_or_else(|| panic!("{name} is gone"));
        assert!(
            new.len() >= old.len() && new[..old.len()] == old[..],
            "{name}: acknowledged bytes changed"
        );
    }
}

/// The frames of the log, read from its files: `(kind, hash, payload)` in log order.
fn frames(dir: &Path) -> Vec<(u8, Hash, Vec<u8>)> {
    let mut out = vec![];
    for (_, bytes) in segments(dir) {
        let mut at = 0usize;
        while at + 37 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            if at + 37 + len > bytes.len() {
                break;
            }
            let hash = Hash(bytes[at + 5..at + 37].try_into().unwrap());
            out.push((bytes[at + 4], hash, bytes[at + 37..at + 37 + len].to_vec()));
            at += 37 + len;
        }
    }
    out
}

/// The `meta/upgrade` ref updates the log holds: one per upgrade batch.
fn upgrade_batches(dir: &Path) -> usize {
    frames(dir)
        .into_iter()
        .filter(|(k, _, p)| {
            *k == kinds::REF_UPDATE
                && postcard::from_bytes::<kinds::RefUpdate>(p).is_ok_and(|u| u.name == REF_UPGRADE)
        })
        .count()
}

/// The hash of the log's last batch marker.
fn terminal(dir: &Path) -> Hash {
    frames(dir)
        .into_iter()
        .rev()
        .find(|(k, _, _)| *k == kinds::BATCH_COMMIT)
        .unwrap()
        .1
}

fn format_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(FORMAT_FILE)).unwrap()
}

fn backup_name(from: &StoreFeatures, terminal: Hash) -> String {
    format!(".store-upgrade-{}-{terminal}", from.hash())
}

/// Where a backup keeps its copy of the store file at `rel`: its `store` directory, beside its
/// own `backup.json`.
fn copy_of(backup: &Path, rel: &str) -> PathBuf {
    backup.join("store").join(rel)
}

fn upgrade_error(e: StoreError) -> (UpgradeFailure, Option<PathBuf>) {
    match e {
        StoreError::Upgrade { failure, backup } => (failure, backup),
        other => panic!("not an upgrade failure: {other:?}"),
    }
}

/// Plans and applies the registry's upgrade on the closed store at `dir`.
fn upgrade(dir: &Path) -> Result<UpgradeReport, StoreError> {
    let lock = StoreLock::take(dir).unwrap();
    let plan = inspect_with(dir, &registry()).unwrap();
    apply_with(dir, &lock, &plan, &registry())
}

/// The engine's completion, as Task 9's engine runs it after its full fsck: the store check on
/// the upgrade's own open, then the mirror.
fn complete(dir: &Path) {
    let lock = StoreLock::take(dir).unwrap();
    let mut s = upgrade::open_pending_for_tests(dir, &lock).unwrap();
    s.check_objects().unwrap();
    upgrade::publish_checked(&mut s).unwrap();
    s.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn ok5_dry_run_writes_nothing() {
    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    // no lock file: a dry run must not make one
    std::fs::remove_file(dir.join("lock")).unwrap();
    let before = tree(base.path());
    let plan = inspect_with(&dir, &registry()).unwrap();
    let bytes: u64 = tree(&dir)
        .iter()
        .filter(|(p, _)| !p.ends_with("lock") && !p.ends_with("index.open"))
        .filter_map(|(_, c)| c.as_ref().map(|c| c.len() as u64))
        .sum();
    assert_eq!(
        plan,
        UpgradePlan {
            from: baseline_features(),
            to: upgraded(),
            steps: STEPS.map(String::from).to_vec(),
            backup_bytes: bytes,
            requires_upgrade: true,
            pending_check: false,
            receipt: None,
        }
    );
    assert_eq!(
        tree(base.path()),
        before,
        "the complete source tree is unchanged"
    );
    assert!(!dir.join("lock").exists(), "no lock file");
    let siblings: Vec<_> = std::fs::read_dir(base.path().join(".rdy"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(siblings, ["store"], "no backup");

    // a dry run reads the same plan while another holder has the store
    let lock = StoreLock::take(&dir).unwrap();
    let before = tree(base.path());
    assert_eq!(inspect_with(&dir, &registry()).unwrap(), plan);
    // this rdy's own steps: none, so nothing to do
    let current = upgrade::inspect(&dir).unwrap();
    assert_eq!(
        (
            current.requires_upgrade,
            current.steps.len(),
            current.backup_bytes
        ),
        (false, 0, 0)
    );
    assert_eq!(current.to, current.from);
    assert_eq!(tree(base.path()), before);
    drop(lock);

    // an unknown incompatible feature still refuses, by the existing format error
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let future = manifest(vec![StoreFeature {
        name: "zz_future".into(),
        class: FeatureClass::Incompatible,
        required_rdy: "99.1.0".into(),
    }]);
    let mut s = Store::create(&dir).unwrap();
    s.set_refs_unchecked_for_tests(vec![(REF_STORE_FEATURES.into(), future.to_json())])
        .unwrap();
    s.close().unwrap();
    format::write_format_file(&dir, &future).unwrap();
    let before = tree(base.path());
    match inspect_with(&dir, &registry()).unwrap_err() {
        StoreError::Format { message, fix } => {
            assert_eq!(message, "store feature zz_future needs rdy 99.1.0");
            assert_eq!(fix, "open the project with rdy 99.1.0 or later");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(tree(base.path()), before);

    // a mirror that differs from the log with no receipt to explain it is refused
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    let other = manifest(vec![feature("zz_a", FeatureClass::Compatible)]);
    s.set_refs_unchecked_for_tests(vec![(REF_STORE_FEATURES.into(), other.to_json())])
        .unwrap();
    s.close().unwrap();
    let before = tree(base.path());
    match inspect_with(&dir, &registry()).unwrap_err() {
        StoreError::Format { message, .. } => assert!(
            message.ends_with("has store features its FORMAT file does not list, and this rdy uses feature-based stores"),
            "{message}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(tree(base.path()), before);
}

/// A frame the upgrade appended: its kind, and a ref update's name and value.
type AddedFrame = (u8, Option<(String, Vec<u8>)>);

#[test]
fn ok5_upgrade_is_one_batch_and_keeps_all_hashes() {
    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    // a kill's leftover sentinel: the backup leaves it out, as it does the lock
    std::fs::write(dir.join("index.open"), b"").unwrap();
    let (refs, tags, landings, frame_count, end) = {
        let s = Store::open(&dir).unwrap();
        let out = (
            s.refs_with_prefix("").unwrap(),
            s.tags().unwrap(),
            s.landings().unwrap(),
            s.verify().unwrap(),
            s.log_end(),
        );
        s.close().unwrap();
        out
    };
    std::fs::write(dir.join("index.open"), b"").unwrap();
    let old_tree = tree(&dir);
    let old_segments = segments(&dir);
    let old_format = format_text(&dir);
    let old_frames = frames(&dir);
    let last = terminal(&dir);
    let name = backup_name(&baseline_features(), last);

    let report = upgrade(&dir).unwrap();
    let receipt = UpgradeReceipt {
        from: baseline_features().hash(),
        to: upgraded().hash(),
        steps: STEPS.map(String::from).to_vec(),
        source_end: end,
        backup: name.clone(),
    };
    let new_frames = frames(&dir);
    let marker = new_frames.last().unwrap().1;
    assert_eq!(
        report,
        UpgradeReport {
            dry_run: false,
            changed: true,
            from: baseline_features(),
            to: upgraded(),
            steps: STEPS.map(String::from).to_vec(),
            backup: Some(base.path().join(".rdy").join(&name)),
            batch: Some(marker),
            reindexed: true,
            checked: false,
            receipt: Some(receipt.clone()),
            pending: false,
        }
    );

    // one batch: the old frames keep their bytes, and only the two refs and a marker follow
    keeps_bytes(&old_segments, &segments(&dir));
    assert_eq!(new_frames[..old_frames.len()], old_frames[..]);
    let added: Vec<AddedFrame> = new_frames[old_frames.len()..]
        .iter()
        .map(|(k, _, p)| {
            let u = (*k == kinds::REF_UPDATE)
                .then(|| postcard::from_bytes::<kinds::RefUpdate>(p).unwrap())
                .map(|u| (u.name, u.value));
            (*k, u)
        })
        .collect();
    assert_eq!(
        added,
        vec![
            (
                kinds::REF_UPDATE,
                Some((REF_STORE_FEATURES.into(), upgraded().to_json()))
            ),
            (
                kinds::REF_UPDATE,
                Some((REF_UPGRADE.into(), receipt.to_json()))
            ),
            (kinds::BATCH_COMMIT, None),
        ]
    );
    assert_eq!(upgrade_batches(&dir), 1);
    // the mirror stays old until the engine's completion
    assert_eq!(format_text(&dir), old_format);

    // the backup is the old store, file for file, with backup.json last
    let backup = base.path().join(".rdy").join(&name);
    let copied = tree(&backup);
    let json: serde_json::Value =
        serde_json::from_slice(copied[&backup.join("backup.json")].as_ref().unwrap()).unwrap();
    assert_eq!(
        json["source_manifest"],
        baseline_features().hash().to_string()
    );
    assert_eq!(json["terminal_batch"], last.to_string());
    let mut listed = vec![];
    for f in json["files"].as_array().unwrap() {
        let path = f["path"].as_str().unwrap();
        let bytes = old_tree[&dir.join(path)].as_ref().unwrap();
        assert_eq!(f["len"], bytes.len() as u64, "{path}");
        assert_eq!(f["blake3"], Hash::of(bytes).to_string(), "{path}");
        assert_eq!(
            copied[&copy_of(&backup, path)].as_ref(),
            Some(bytes),
            "{path}"
        );
        listed.push(path.to_string());
    }
    let mut want: Vec<String> = old_tree
        .iter()
        .filter(|(_, c)| c.is_some())
        .map(|(p, _)| p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned())
        .filter(|p| p != "lock" && p != "index.open")
        .collect();
    want.sort();
    assert_eq!(
        listed, want,
        "every regular file but the lock and the sentinel, in path order"
    );
    assert!(listed.iter().any(|p| p == "FORMAT") && listed.iter().any(|p| p == "index.redb"));
    assert!(listed.iter().any(|p| p.starts_with("segments/")));

    // the refs, tags and objects are the old ones, plus the manifest and the receipt
    let lock = StoreLock::take(&dir).unwrap();
    let s = upgrade::open_pending_for_tests(&dir, &lock).unwrap();
    let mut want_refs = refs.clone();
    for (name, value) in [
        (REF_STORE_FEATURES, upgraded().to_json()),
        (REF_UPGRADE, receipt.to_json()),
    ] {
        want_refs.retain(|(n, _)| n != name);
        want_refs.push((name.into(), value));
    }
    want_refs.sort();
    let mut got_refs = s.refs_with_prefix("").unwrap();
    got_refs.sort();
    assert_eq!(got_refs, want_refs);
    assert_eq!(s.tags().unwrap(), tags);
    assert_eq!(s.landings().unwrap(), landings);
    for (h, rec) in &landings {
        assert_eq!(&s.landing(h).unwrap(), rec);
        assert_eq!(
            rdy_store::codec::logical_kind(s.get_raw(h).unwrap().0),
            kinds::LANDING
        );
    }
    // the two ref frames (verify counts data frames, not markers)
    assert_eq!(s.verify().unwrap(), frame_count + 2);
    s.check_objects().unwrap();
    s.close().unwrap();
    drop(lock);

    complete(&dir);
    assert_eq!(format_text(&dir), upgraded().to_text());
    let after = upgrade::inspect(&dir).unwrap();
    assert_eq!(
        (
            after.pending_check,
            after.requires_upgrade,
            after.from.clone()
        ),
        (false, false, upgraded())
    );
    assert_eq!(after.receipt, Some(receipt));
    // a normal open now reads the upgraded store
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.features(), &upgraded());
    assert_eq!(s.tags().unwrap(), tags);
    s.close().unwrap();
    assert_eq!(upgrade_batches(&dir), 1);
}

#[test]
fn ok5_backup_is_complete_before_append() {
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    let old_segments = segments(&dir);
    let old_tree = tree(&dir);
    let name = backup_name(&baseline_features(), terminal(&dir));
    let backup = base.path().join(".rdy").join(&name);
    // the worker says when it reaches the open store, and waits there for the release; it
    // also says when its upgrade returns, so a run that never pauses fails instead of hanging
    let (said, heard) = std::sync::mpsc::channel::<&'static str>();
    let release = Arc::new(Barrier::new(2));
    let worker = {
        let (dir, release) = (dir.clone(), release.clone());
        std::thread::spawn(move || {
            let _f = known();
            let at_open = said.clone();
            let r = with_hook_for_tests(
                move |p: Phase<'_>| {
                    if let Phase::Opened(_) = p {
                        at_open.send("opened").unwrap();
                        release.wait();
                    }
                    Ok(())
                },
                || upgrade(&dir),
            );
            said.send("returned").unwrap();
            r
        })
    };
    assert_eq!(
        heard.recv().unwrap(),
        "opened",
        "the upgrade stops at the barrier before its first append"
    );
    // the store is open and nothing is appended: the backup is already complete and durable
    assert_eq!(segments(&dir), old_segments, "nothing appended yet");
    assert_eq!(upgrade_batches(&dir), 0);
    let copied = tree(&backup);
    let json: serde_json::Value =
        serde_json::from_slice(copied[&backup.join("backup.json")].as_ref().unwrap()).unwrap();
    let files = json["files"].as_array().unwrap();
    assert!(!files.is_empty());
    for f in files {
        let path = f["path"].as_str().unwrap();
        let bytes = copied[&copy_of(&backup, path)].as_ref().unwrap();
        assert_eq!(f["blake3"], Hash::of(bytes).to_string(), "{path}");
        assert_eq!(old_tree[&dir.join(path)].as_ref(), Some(bytes), "{path}");
    }
    release.wait();
    assert_eq!(heard.recv().unwrap(), "returned");
    let report = worker.join().unwrap().unwrap();
    assert!(report.changed);
    assert_eq!(upgrade_batches(&dir), 1);
}

/// A fault an upgrade meets, and whether it comes after the seal.
#[derive(Clone, Copy, Debug)]
enum Fault {
    Copy,
    Sync,
    Append,
    Seal,
    IndexApply,
    Reindex,
}

fn inject(fault: Fault) -> impl for<'a> FnMut(Phase<'a>) -> std::io::Result<()> + 'static {
    let mut copies = 0;
    move |p: Phase<'_>| {
        match (fault, p) {
            (Fault::Copy, Phase::Copy(_)) => {
                copies += 1;
                if copies == 2 {
                    return Err(std::io::Error::other("injected copy failure"));
                }
            }
            (Fault::Sync, Phase::Sync(rel)) if rel.starts_with("segments/") => {
                return Err(std::io::Error::other("injected sync failure"));
            }
            (Fault::Append, Phase::Opened(s)) => s.fail_next_append_after(0),
            (Fault::Seal, Phase::Opened(s)) => s.fail_next_sync(),
            (Fault::IndexApply, Phase::Opened(s)) => s.fail_next_index_apply(),
            (Fault::Reindex, Phase::Sealed(s)) => s.fail_next_replay_after(0),
            _ => {}
        }
        Ok(())
    }
}

#[test]
fn ok5_each_failure_keeps_acknowledged_bytes() {
    let _f = known();
    for fault in [
        Fault::Copy,
        Fault::Sync,
        Fault::Append,
        Fault::Seal,
        Fault::IndexApply,
        Fault::Reindex,
    ] {
        let base = tempfile::tempdir().unwrap();
        let dir = project(base.path());
        let old_segments = segments(&dir);
        let old_format = format_text(&dir);
        let plan = inspect_with(&dir, &registry()).unwrap();
        let name = backup_name(&baseline_features(), terminal(&dir));
        let backup = base.path().join(".rdy").join(&name);
        let e = with_hook_for_tests(inject(fault), || upgrade(&dir)).unwrap_err();
        assert!(
            matches!(e, StoreError::Io(_) | StoreError::Db(_)),
            "{fault:?}: an I/O or index failure keeps its own error (E_IO): {e:?}"
        );
        keeps_bytes(&old_segments, &segments(&dir));
        assert_eq!(
            format_text(&dir),
            old_format,
            "{fault:?}: FORMAT is untouched"
        );
        assert!(backup.is_dir(), "{fault:?}: the backup is kept");
        if let Fault::IndexApply | Fault::Reindex = fault {
            // after the seal: the marker's sync is the upgrade's commit point, so an index
            // update that fails after it keeps the batch too. The batch is durable, the index
            // is rebuilt from the log, and the receipt waits for its completion
            assert_eq!(upgrade_batches(&dir), 1, "{fault:?}");
            let now = inspect_with(&dir, &registry()).unwrap();
            assert!(
                now.pending_check && now.steps.is_empty(),
                "{fault:?}: {now:?}"
            );
            assert_eq!(now.receipt.as_ref().unwrap().backup, name);
            let lock = StoreLock::take(&dir).unwrap();
            let again = apply_with(&dir, &lock, &now, &registry()).unwrap();
            assert_eq!(
                (again.changed, again.batch, again.backup),
                (false, None, None)
            );
            drop(lock);
            assert_eq!(upgrade_batches(&dir), 1, "no second batch");
            complete(&dir);
            assert_eq!(format_text(&dir), upgraded().to_text());
            continue;
        }
        // before the seal: the rollback left exactly the acknowledged bytes, and the plan stands
        assert_eq!(
            segments(&dir),
            old_segments,
            "{fault:?}: nothing more is kept"
        );
        assert_eq!(upgrade_batches(&dir), 0, "{fault:?}");
        assert_eq!(inspect_with(&dir, &registry()).unwrap(), plan, "{fault:?}");
        let complete_backup = backup.join("backup.json").exists();
        assert_eq!(
            complete_backup,
            !matches!(fault, Fault::Copy | Fault::Sync),
            "{fault:?}: backup.json is written last"
        );
        if !complete_backup {
            // an incomplete backup is never used: it is moved aside by a person
            let e = upgrade(&dir).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!(
                    "the upgrade backup is incomplete or does not match the store; backup: {name}"
                )
            );
            assert_eq!(
                upgrade_error(e),
                (UpgradeFailure::BackupMismatch, Some(backup.clone()))
            );
            assert_eq!(segments(&dir), old_segments, "{fault:?}");
        }
        std::fs::rename(&backup, base.path().join("aside")).unwrap();
        let report = upgrade(&dir).unwrap();
        assert!(report.changed, "{fault:?}");
        assert_eq!(upgrade_batches(&dir), 1, "{fault:?}");
        keeps_bytes(&old_segments, &segments(&dir));
    }
}

/// The child of `ok5_restart_retains_receipt_without_second_batch`: upgrades the store at
/// `RDY_OK5_DIR`, prints `OK5-PHASE <name>` at phase `RDY_OK5_PHASE` (at the third copy for
/// `copy`) and waits there to be killed.
#[test]
#[ignore]
fn ok5_upgrade_child() {
    let Ok(dir) = std::env::var("RDY_OK5_DIR") else {
        return;
    };
    let phase = std::env::var("RDY_OK5_PHASE").unwrap();
    let _f = known();
    let mut copies = 0;
    let r = with_hook_for_tests(
        move |p: Phase<'_>| {
            let name = p.name();
            if name == "copy" {
                copies += 1;
            }
            if name == phase && (name != "copy" || copies == 3) {
                println!("OK5-PHASE {name}");
                std::io::stdout().flush().unwrap();
                loop {
                    std::thread::park();
                }
            }
            Ok(())
        },
        || upgrade(Path::new(&dir)),
    );
    println!("OK5-DONE {r:?}");
}

/// Runs the child on `dir` and kills it with SIGKILL once it reports `phase`.
fn kill_at(dir: &Path, phase: &str) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "ok5_upgrade_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("RDY_OK5_DIR", dir)
        .env("RDY_OK5_PHASE", phase)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            out.read_line(&mut line).unwrap() > 0,
            "the child ended before phase {phase}"
        );
        if line.contains(&format!("OK5-PHASE {phase}")) {
            break;
        }
    }
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(9), "{phase}: {status:?}");
}

#[test]
fn ok5_restart_retains_receipt_without_second_batch() {
    let _f = known();
    for phase in [
        "copy",
        "backup_complete",
        "opened",
        "sealed",
        "reindexed",
        "checked",
    ] {
        let base = tempfile::tempdir().unwrap();
        let dir = project(base.path());
        let old_segments = segments(&dir);
        let old_format = format_text(&dir);
        let end = Store::open(&dir).unwrap().log_end();
        let name = backup_name(&baseline_features(), terminal(&dir));
        let backup = base.path().join(".rdy").join(&name);
        kill_at(&dir, phase);
        keeps_bytes(&old_segments, &segments(&dir));
        assert_eq!(
            format_text(&dir),
            old_format,
            "{phase}: the mirror stays old"
        );
        let plan = inspect_with(&dir, &registry()).unwrap();
        if matches!(phase, "copy" | "backup_complete" | "opened") {
            // before the seal: nothing was appended, and the same upgrade is planned
            assert_eq!(segments(&dir), old_segments, "{phase}");
            assert_eq!(upgrade_batches(&dir), 0, "{phase}");
            assert!(
                !plan.pending_check && plan.steps == STEPS,
                "{phase}: {plan:?}"
            );
            let lock = StoreLock::take(&dir).unwrap();
            let report = match apply_with(&dir, &lock, &plan, &registry()) {
                // a backup the kill left complete, of the unchanged store, is used as it is
                Ok(report) => {
                    assert_eq!(phase, "backup_complete", "only a complete backup is reused");
                    report
                }
                Err(e) => {
                    assert_ne!(phase, "backup_complete");
                    assert_eq!(
                        upgrade_error(e),
                        (UpgradeFailure::BackupMismatch, Some(backup.clone())),
                        "{phase}"
                    );
                    std::fs::rename(&backup, base.path().join("aside")).unwrap();
                    apply_with(&dir, &lock, &plan, &registry()).unwrap()
                }
            };
            assert!(report.changed, "{phase}");
            drop(lock);
            assert_eq!(upgrade_batches(&dir), 1, "{phase}");
            assert_eq!(format_text(&dir), old_format, "{phase}");
            complete(&dir);
            assert_eq!(format_text(&dir), upgraded().to_text(), "{phase}");
            continue;
        }
        // after the seal: the batch and its receipt are durable, and nothing is planned again
        assert_eq!(upgrade_batches(&dir), 1, "{phase}");
        assert!(backup.join("backup.json").exists(), "{phase}");
        assert!(
            plan.pending_check && plan.steps.is_empty(),
            "{phase}: {plan:?}"
        );
        assert_eq!(
            (plan.from.clone(), plan.to.clone()),
            (upgraded(), upgraded())
        );
        let receipt = plan.receipt.clone().unwrap();
        assert_eq!(
            receipt,
            UpgradeReceipt {
                from: baseline_features().hash(),
                to: upgraded().hash(),
                steps: STEPS.map(String::from).to_vec(),
                source_end: end,
                backup: name.clone(),
            }
        );
        let lock = StoreLock::take(&dir).unwrap();
        let again = apply_with(&dir, &lock, &plan, &registry()).unwrap();
        assert_eq!(
            again,
            UpgradeReport {
                dry_run: false,
                changed: false,
                from: upgraded(),
                to: upgraded(),
                steps: vec![],
                backup: None,
                batch: None,
                reindexed: false,
                checked: false,
                receipt: Some(receipt.clone()),
                pending: false,
            },
            "{phase}"
        );
        drop(lock);
        assert_eq!(upgrade_batches(&dir), 1, "{phase}: no second batch");
        assert_eq!(
            format_text(&dir),
            old_format,
            "{phase}: still old before completion"
        );
        complete(&dir);
        assert_eq!(format_text(&dir), upgraded().to_text(), "{phase}");
        let done = inspect_with(&dir, &registry()).unwrap();
        assert!(!done.pending_check && done.steps.is_empty(), "{phase}");
        assert_eq!(done.receipt, Some(receipt));
        assert_eq!(upgrade_batches(&dir), 1, "{phase}");
    }
}

#[test]
fn ok5_noop_has_no_backup_or_batch() {
    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    let lock = StoreLock::take(&dir).unwrap();
    let before = tree(base.path());
    // this rdy's registry is empty: a current store needs no step
    let plan = upgrade::inspect(&dir).unwrap();
    let noop = |from: StoreFeatures, receipt: Option<UpgradeReceipt>| UpgradeReport {
        dry_run: false,
        changed: false,
        from: from.clone(),
        to: from,
        steps: vec![],
        backup: None,
        batch: None,
        reindexed: false,
        checked: false,
        receipt,
        pending: false,
    };
    assert_eq!(
        upgrade::apply_locked(&dir, &lock, &plan).unwrap(),
        noop(baseline_features(), None)
    );
    assert_eq!(
        tree(base.path()),
        before,
        "no backup, no batch, no file changed"
    );
    // a lock that is not this store's is refused before anything is read
    let other = tempfile::tempdir().unwrap();
    let foreign = StoreLock::take(other.path()).unwrap();
    assert!(matches!(
        upgrade::apply_locked(&dir, &foreign, &plan),
        Err(StoreError::Locked(_))
    ));
    drop(lock);

    // after an upgrade, the same registry has nothing left to do, before and after completion
    let stale = inspect_with(&dir, &registry()).unwrap();
    let first = upgrade(&dir).unwrap();
    let receipt = first.receipt.clone();
    let lock = StoreLock::take(&dir).unwrap();
    let pending = inspect_with(&dir, &registry()).unwrap();
    assert!(pending.pending_check);
    let before = tree(base.path());
    assert_eq!(
        apply_with(&dir, &lock, &pending, &registry()).unwrap(),
        noop(upgraded(), receipt.clone())
    );
    assert_eq!(tree(base.path()), before);
    // the plan made before the upgrade no longer fits the store
    assert_eq!(
        upgrade_error(apply_with(&dir, &lock, &stale, &registry()).unwrap_err()),
        (UpgradeFailure::Moved, None)
    );
    assert_eq!(tree(base.path()), before);
    drop(lock);
    complete(&dir);
    let lock = StoreLock::take(&dir).unwrap();
    let before = tree(base.path());
    let current = inspect_with(&dir, &registry()).unwrap();
    assert!(!current.pending_check && !current.requires_upgrade);
    assert_eq!(
        apply_with(&dir, &lock, &current, &registry()).unwrap(),
        noop(upgraded(), receipt)
    );
    assert_eq!(tree(base.path()), before);
    assert_eq!(upgrade_batches(&dir), 1);
}

fn holder_prov(at: i64) -> Provenance {
    Provenance {
        actor: "human:a".into(),
        run: None,
        role: Role::Approver,
        at_ms: at,
    }
}

/// A closed holder at `<base>/h` with one tagged landing; `with_block` false leaves the landed
/// block unwritten, which only the holder check finds.
fn holder(base: &Path, with_block: bool) -> PathBuf {
    let dir = base.join("h");
    let mut s = Store::create_holder(&dir, RepoId([7; 16])).unwrap();
    let b = block("b-00001");
    if with_block {
        s.put(kinds::BLOCK, &b).unwrap();
    }
    let change = Change {
        view: "work".into(),
        ops: vec![Op::Create { block: b.clone() }],
        deps: vec![],
        provenance: holder_prov(1),
        justification: None,
        message: "c".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    let c = s.put(kinds::CHANGE, &change).unwrap();
    let state = StateSnapshot {
        blocks: [(b.id.clone(), b.version_hash())].into_iter().collect(),
    };
    let state = s.put(kinds::SNAPSHOT, &state).unwrap();
    let proposal = s
        .put(
            kinds::PROPOSAL,
            &Proposal {
                stable: None,
                prev: None,
                title: "t".into(),
                view: "work".into(),
                changes: vec![c],
                alternative: None,
                provenance: holder_prov(2),
                canon_at: None,
                summary: Some("s".into()),
            },
        )
        .unwrap();
    let landing = s
        .put(
            kinds::LANDING,
            &LandingRecord {
                proposal,
                prev: None,
                state,
                approved_by: holder_prov(3),
                changes: vec![c],
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
            },
        )
        .unwrap();
    let t = s
        .put(
            kinds::TAG,
            &Tag {
                name: "v1.0.0".into(),
                landing,
                message: None,
                tagged_by: holder_prov(4),
            },
        )
        .unwrap();
    s.set_refs(vec![("tag/v1.0.0".into(), t.0.to_vec())])
        .unwrap();
    s.close().unwrap();
    dir
}

#[test]
fn ok5_holder_uses_holder_store_check() {
    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let dir = holder(base.path(), true);
    let refs = {
        let s = Store::open(&dir).unwrap();
        s.check_holder().unwrap();
        s.refs_with_prefix("").unwrap()
    };
    let name = backup_name(&baseline_features(), terminal(&dir));
    let report = upgrade(&dir).unwrap();
    assert!(report.changed && report.reindexed && !report.checked);
    assert_eq!(report.backup, Some(base.path().join(&name)));
    let lock = StoreLock::take(&dir).unwrap();
    let s = upgrade::open_pending_for_tests(&dir, &lock).unwrap();
    // the holder's refs stay closed: the old ones, the new manifest and the receipt
    let mut want: Vec<String> = refs.iter().map(|(n, _)| n.clone()).collect();
    want.push(REF_UPGRADE.into());
    want.sort();
    let mut got: Vec<String> = s
        .refs_with_prefix("")
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    got.sort();
    assert_eq!(got, want);
    for (name, value) in &refs {
        if name != REF_STORE_FEATURES {
            assert_eq!(s.get_ref(name).unwrap().as_ref(), Some(value), "{name}");
        }
    }
    assert_eq!(s.mode().unwrap(), rdy_store::holder::StoreMode::Holder);
    s.check_holder().unwrap();
    let mut s = s;
    upgrade::publish_checked(&mut s).unwrap();
    s.close().unwrap();
    drop(lock);
    assert_eq!(format_text(&dir), upgraded().to_text());

    // a holder whose tag reaches a missing block passes the project check but not the holder's:
    // the upgrade's check is the holder's, and its failure keeps the batch and the backup
    let base = tempfile::tempdir().unwrap();
    let dir = holder(base.path(), false);
    let old_format = format_text(&dir);
    let name = backup_name(&baseline_features(), terminal(&dir));
    {
        let s = Store::open(&dir).unwrap();
        assert!(s.check_holder().is_err());
    }
    let e = upgrade(&dir).unwrap_err();
    assert_eq!(
        e.to_string(),
        format!("the upgrade batch is durable, but the store check failed; backup: {name}")
    );
    assert_eq!(
        upgrade_error(e),
        (UpgradeFailure::CheckFailed, Some(base.path().join(&name)))
    );
    assert_eq!(upgrade_batches(&dir), 1, "the batch is durable");
    assert!(base.path().join(&name).join("backup.json").exists());
    assert_eq!(format_text(&dir), old_format, "no mirror is published");
    let after = inspect_with(&dir, &registry()).unwrap();
    assert!(after.pending_check && after.steps.is_empty());
}

#[test]
fn ok5_backup_limits_and_links_fail_closed() {
    let _f = known();
    let refused = |dir: &Path, plan: &UpgradePlan, want: UpgradeFailure| {
        let before = tree(dir.parent().unwrap());
        let lock = StoreLock::take(dir).unwrap();
        assert_eq!(
            upgrade_error(inspect_with(dir, &registry()).unwrap_err()),
            (want, None),
            "a dry run refuses too"
        );
        let e = apply_with(dir, &lock, plan, &registry()).unwrap_err();
        assert_eq!(upgrade_error(e), (want, None));
        drop(lock);
        assert_eq!(tree(dir.parent().unwrap()), before, "no backup, no batch");
        assert_eq!(upgrade_batches(dir), 0);
    };
    let fresh = || {
        let base = tempfile::tempdir().unwrap();
        let dir = project(base.path());
        let plan = inspect_with(&dir, &registry()).unwrap();
        (base, dir, plan)
    };

    // a link to a file, a link to a directory and a socket, at the top and deeper
    let (_b, dir, plan) = fresh();
    std::os::unix::fs::symlink(dir.join("FORMAT"), dir.join("FORMAT.link")).unwrap();
    refused(&dir, &plan, UpgradeFailure::BackupFileType);
    let (b, dir, plan) = fresh();
    std::os::unix::fs::symlink(b.path(), dir.join("segments").join("elsewhere")).unwrap();
    refused(&dir, &plan, UpgradeFailure::BackupFileType);
    let (_b, dir, plan) = fresh();
    std::fs::create_dir(dir.join("extra")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(dir.join("extra").join("s")).unwrap();
    refused(&dir, &plan, UpgradeFailure::BackupFileType);

    // the entry and path bounds, each refused one past and accepted at its value
    let (b, dir, plan) = fresh();
    std::fs::create_dir_all(dir.join("extra").join("deeper")).unwrap();
    std::fs::write(dir.join("extra").join("deeper").join("note"), b"kept").unwrap();
    let plan = UpgradePlan {
        backup_bytes: plan.backup_bytes + 4,
        ..plan
    };
    let rels: Vec<String> = tree(&dir)
        .keys()
        .map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned())
        .filter(|p| p != "lock" && p != "index.open")
        .collect();
    let entries = rels.len();
    let longest = rels.iter().map(String::len).max().unwrap();
    assert_eq!(
        longest,
        "extra/deeper/note"
            .len()
            .max("segments/seg-000000.log".len())
    );
    with_backup_limits_for_tests(entries - 1, MAX_BACKUP_PATH_BYTES, || {
        refused(&dir, &plan, UpgradeFailure::Limit)
    });
    with_backup_limits_for_tests(MAX_BACKUP_ENTRIES, longest - 1, || {
        refused(&dir, &plan, UpgradeFailure::Limit)
    });
    let report = with_backup_limits_for_tests(entries, longest, || {
        assert_eq!(inspect_with(&dir, &registry()).unwrap(), plan);
        upgrade(&dir).unwrap()
    });
    // the extra regular file is copied with the store's
    let backup = report.backup.unwrap();
    assert_eq!(
        std::fs::read(copy_of(&backup, "extra/deeper/note")).unwrap(),
        b"kept"
    );
    assert!(
        b.path()
            .join(".rdy")
            .join(backup.file_name().unwrap())
            .is_dir()
    );
    assert!(!copy_of(&backup, "lock").exists() && !backup.join("lock").exists());

    // a complete backup of the unchanged store is reused, but not once it holds a link
    let (b, dir, plan) = fresh();
    let name = backup_name(&baseline_features(), terminal(&dir));
    let backup = b.path().join(".rdy").join(&name);
    let stop = |p: Phase<'_>| match p {
        Phase::BackupComplete => Err(std::io::Error::other("stop after the backup")),
        _ => Ok(()),
    };
    let e = with_hook_for_tests(stop, || upgrade(&dir)).unwrap_err();
    assert!(matches!(e, StoreError::Io(_)), "{e:?}");
    std::os::unix::fs::symlink(dir.join("FORMAT"), backup.join("planted")).unwrap();
    let lock = StoreLock::take(&dir).unwrap();
    assert_eq!(
        upgrade_error(apply_with(&dir, &lock, &plan, &registry()).unwrap_err()),
        (UpgradeFailure::BackupMismatch, Some(backup.clone()))
    );
    assert!(
        backup.join("planted").exists(),
        "nothing in a backup changes"
    );
    assert_eq!(upgrade_batches(&dir), 0);
    std::fs::remove_file(backup.join("planted")).unwrap();
    // a link in the copy, and a stray entry in place of the copy, are refused too
    std::os::unix::fs::symlink(dir.join("FORMAT"), copy_of(&backup, "planted")).unwrap();
    assert_eq!(
        upgrade_error(apply_with(&dir, &lock, &plan, &registry()).unwrap_err()),
        (UpgradeFailure::BackupMismatch, Some(backup.clone()))
    );
    std::fs::remove_file(copy_of(&backup, "planted")).unwrap();
    std::fs::rename(backup.join("store"), backup.join("stray")).unwrap();
    assert_eq!(
        upgrade_error(apply_with(&dir, &lock, &plan, &registry()).unwrap_err()),
        (UpgradeFailure::BackupMismatch, Some(backup.clone()))
    );
    std::fs::rename(backup.join("stray"), backup.join("store")).unwrap();
    assert_eq!(upgrade_batches(&dir), 0);
    let report = apply_with(&dir, &lock, &plan, &registry()).unwrap();
    assert_eq!(report.backup, Some(backup));
    assert_eq!(upgrade_batches(&dir), 1);
}

/// The mode bits of `path`, never through a link.
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// The entry names of the directory `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// Stops an upgrade once its backup is complete, before the store opens.
fn stop_at_backup(p: Phase<'_>) -> std::io::Result<()> {
    match p {
        Phase::BackupComplete => Err(std::io::Error::other("stop after the backup")),
        _ => Ok(()),
    }
}

#[test]
fn ok5_backup_keeps_store_files_apart_from_its_manifest() {
    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    // store files with the names of the backup's own manifest and of its temp file
    std::fs::write(dir.join("backup.json"), b"a person's file").unwrap();
    std::fs::write(dir.join(".backup.json.tmp"), b"another person's file").unwrap();
    let old_tree = tree(&dir);
    let report = upgrade(&dir).unwrap();
    assert!(report.changed);
    let backup = report.backup.unwrap();
    // the backup holds its manifest and the copy, apart
    assert_eq!(names(&backup), ["backup.json", "store"]);
    for rel in ["backup.json", ".backup.json.tmp"] {
        assert_eq!(
            std::fs::read(copy_of(&backup, rel)).ok().as_ref(),
            old_tree[&dir.join(rel)].as_ref(),
            "{rel}: the copied file keeps its bytes"
        );
    }
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(backup.join("backup.json")).unwrap()).unwrap();
    let files = json["files"].as_array().unwrap();
    for rel in ["backup.json", ".backup.json.tmp"] {
        assert!(files.iter().any(|f| f["path"] == rel), "{rel} is listed");
    }
    // every listed file's copy has its recorded digest and its source's bytes
    for f in files {
        let path = f["path"].as_str().unwrap();
        let copy = std::fs::read(copy_of(&backup, path)).unwrap();
        assert_eq!(f["blake3"], Hash::of(&copy).to_string(), "{path}");
        assert_eq!(old_tree[&dir.join(path)].as_ref(), Some(&copy), "{path}");
    }
}

#[test]
fn ok5_backup_is_private_to_its_owner() {
    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    set_mode(&dir, 0o700);
    set_mode(&dir.join("FORMAT"), 0o600);
    set_mode(&dir.join("index.redb"), 0o400);
    let report = upgrade(&dir).unwrap();
    let backup = report.backup.unwrap();
    // no group or other bit on the backup or on anything in it
    assert_eq!(mode(&backup) & 0o077, 0, "{:o}", mode(&backup));
    let all = tree(&backup);
    assert!(all.len() > 4);
    for p in all.keys() {
        assert_eq!(mode(p) & 0o077, 0, "{}: {:o}", p.display(), mode(p));
    }
    // a file keeps no more of its owner's bits than its source has
    assert_eq!(mode(&copy_of(&backup, "FORMAT")), 0o600);
    assert_eq!(mode(&copy_of(&backup, "index.redb")), 0o400);

    // a backup that other users can read is not reused
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    let name = backup_name(&baseline_features(), terminal(&dir));
    let backup = base.path().join(".rdy").join(&name);
    let plan = inspect_with(&dir, &registry()).unwrap();
    with_hook_for_tests(stop_at_backup, || upgrade(&dir)).unwrap_err();
    let lock = StoreLock::take(&dir).unwrap();
    for (path, open, private) in [
        (copy_of(&backup, "FORMAT"), 0o644, 0o600),
        (backup.join("store").join("segments"), 0o755, 0o700),
        (backup.clone(), 0o755, 0o700),
    ] {
        set_mode(&path, open);
        assert_eq!(
            upgrade_error(apply_with(&dir, &lock, &plan, &registry()).unwrap_err()),
            (UpgradeFailure::BackupMismatch, Some(backup.clone())),
            "{}",
            path.display()
        );
        assert_eq!(upgrade_batches(&dir), 0);
        set_mode(&path, private);
    }
    let report = apply_with(&dir, &lock, &plan, &registry()).unwrap();
    assert_eq!(report.backup, Some(backup));
    assert_eq!(upgrade_batches(&dir), 1);
}

#[test]
fn ok5_backup_never_follows_a_substituted_path() {
    let _f = known();
    // the new backup directory is moved and a link to another directory takes its name
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    let old_segments = segments(&dir);
    let backup = base
        .path()
        .join(".rdy")
        .join(backup_name(&baseline_features(), terminal(&dir)));
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("backup.json"), b"keep me").unwrap();
    let swap = {
        let (backup, outside, moved) = (
            backup.clone(),
            outside.path().to_path_buf(),
            base.path().join("moved"),
        );
        let mut done = false;
        move |p: Phase<'_>| {
            if let Phase::Copy(_) = p
                && !done
            {
                done = true;
                std::fs::rename(&backup, &moved)?;
                std::os::unix::fs::symlink(&outside, &backup)?;
            }
            Ok(())
        }
    };
    let e = with_hook_for_tests(swap, || upgrade(&dir)).unwrap_err();
    assert_eq!(
        upgrade_error(e),
        (UpgradeFailure::BackupMismatch, Some(backup.clone()))
    );
    assert_eq!(names(outside.path()), ["backup.json"], "nothing is added");
    assert_eq!(
        std::fs::read(outside.path().join("backup.json")).unwrap(),
        b"keep me"
    );
    assert_eq!(segments(&dir), old_segments, "nothing is appended");
    assert_eq!(upgrade_batches(&dir), 0);

    // a listed store file is replaced by a link just before its copy
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    std::fs::write(dir.join("note"), b"mine").unwrap();
    let old_segments = segments(&dir);
    let backup = base
        .path()
        .join(".rdy")
        .join(backup_name(&baseline_features(), terminal(&dir)));
    let secret = outside.path().join("secret");
    std::fs::write(&secret, b"not the store's").unwrap();
    let swap = {
        let (note, secret) = (dir.join("note"), secret.clone());
        move |p: Phase<'_>| {
            if let Phase::Copy("note") = p {
                std::fs::remove_file(&note)?;
                std::os::unix::fs::symlink(&secret, &note)?;
            }
            Ok(())
        }
    };
    let e = with_hook_for_tests(swap, || upgrade(&dir)).unwrap_err();
    assert_eq!(
        upgrade_error(e),
        (UpgradeFailure::BackupFileType, Some(backup.clone()))
    );
    assert!(
        tree(&backup)
            .values()
            .all(|c| c.as_deref() != Some(&b"not the store's"[..])),
        "the link's target is not copied"
    );
    assert!(
        !backup.join("backup.json").exists(),
        "the backup is not complete"
    );
    assert_eq!(segments(&dir), old_segments, "nothing is appended");
    assert_eq!(upgrade_batches(&dir), 0);
}

#[test]
fn ok5_backup_copy_directory_is_never_substituted() {
    let _f = known();
    // at the first copy, the backup's copy directory is moved away, and a link to an empty
    // directory, or another empty directory, takes its name: the copy goes on through its
    // handle, so backup.json would describe files that are not at the backup's path
    for by_link in [true, false] {
        let base = tempfile::tempdir().unwrap();
        let dir = project(base.path());
        let old_segments = segments(&dir);
        let backup = base
            .path()
            .join(".rdy")
            .join(backup_name(&baseline_features(), terminal(&dir)));
        let empty = tempfile::tempdir().unwrap();
        let swap = {
            let (copy, moved, empty) = (
                backup.join("store"),
                base.path().join("moved"),
                empty.path().to_path_buf(),
            );
            let mut done = false;
            move |p: Phase<'_>| {
                if let Phase::Copy(_) = p
                    && !done
                {
                    done = true;
                    std::fs::rename(&copy, &moved)?;
                    if by_link {
                        std::os::unix::fs::symlink(&empty, &copy)?;
                    } else {
                        std::fs::create_dir(&copy)?;
                        set_mode(&copy, 0o700);
                    }
                }
                Ok(())
            }
        };
        let e = with_hook_for_tests(swap, || upgrade(&dir)).unwrap_err();
        assert_eq!(
            upgrade_error(e),
            (UpgradeFailure::BackupMismatch, Some(backup.clone())),
            "by link: {by_link}"
        );
        assert!(names(empty.path()).is_empty(), "by link: {by_link}");
        assert!(
            names(&backup.join("store")).is_empty(),
            "by link: {by_link}"
        );
        assert_eq!(segments(&dir), old_segments, "nothing is appended");
        assert_eq!(upgrade_batches(&dir), 0);
        assert!(!inspect_with(&dir, &registry()).unwrap().pending_check);
    }
}

#[test]
fn ok5_reused_backup_refuses_hard_links_and_a_linked_manifest() {
    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    let name = backup_name(&baseline_features(), terminal(&dir));
    let backup = base.path().join(".rdy").join(&name);
    let plan = inspect_with(&dir, &registry()).unwrap();
    with_hook_for_tests(stop_at_backup, || upgrade(&dir)).unwrap_err();
    let seg = "segments/seg-000000.log";
    let copy = copy_of(&backup, seg);
    let saved = std::fs::read(&copy).unwrap();
    let old_segments = segments(&dir);
    let lock = StoreLock::take(&dir).unwrap();

    // the copied segment is replaced by a hard link to the active one, with a copy's mode, so
    // the same bytes and the same privacy: only its identity tells it from a copy
    set_mode(&dir.join(seg), 0o600);
    std::fs::remove_file(&copy).unwrap();
    std::fs::hard_link(dir.join(seg), &copy).unwrap();
    assert_eq!(
        upgrade_error(apply_with(&dir, &lock, &plan, &registry()).unwrap_err()),
        (UpgradeFailure::BackupMismatch, Some(backup.clone()))
    );
    assert_eq!(segments(&dir), old_segments, "nothing is appended");
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        saved,
        "the backup's bytes stay"
    );
    assert_eq!(upgrade_batches(&dir), 0);
    std::fs::remove_file(&copy).unwrap();
    std::fs::write(&copy, &saved).unwrap();
    set_mode(&copy, 0o600);

    // backup.json is moved out and a link to it takes its place
    let manifest = backup.join("backup.json");
    let outside = tempfile::tempdir().unwrap();
    let moved = outside.path().join("backup.json");
    std::fs::rename(&manifest, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &manifest).unwrap();
    assert_eq!(
        upgrade_error(apply_with(&dir, &lock, &plan, &registry()).unwrap_err()),
        (UpgradeFailure::BackupMismatch, Some(backup.clone()))
    );
    assert_eq!(upgrade_batches(&dir), 0);
    std::fs::remove_file(&manifest).unwrap();
    std::fs::rename(&moved, &manifest).unwrap();

    // the independent, complete backup is reused, and the upgrade's append leaves it as it was
    let report = apply_with(&dir, &lock, &plan, &registry()).unwrap();
    assert_eq!(report.backup, Some(backup.clone()));
    assert_eq!(upgrade_batches(&dir), 1);
    assert!(segments(&dir)[&seg["segments/".len()..]].len() > saved.len());
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        saved,
        "the backup's bytes stay"
    );
}

#[test]
fn ok12_a_live_reader_is_refused_before_the_backup() {
    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    let old = format_text(&dir);
    // a reader that outlived its store still holds the index file; the baseline is taken after
    // the open and close that made it
    let reader = Store::open(&dir).unwrap().reader();
    let store_tree = tree(&dir);
    let lock = StoreLock::take(&dir).unwrap();
    let plan = inspect_with(&dir, &registry()).unwrap();
    let e = apply_with(&dir, &lock, &plan, &registry()).unwrap_err();
    assert!(
        matches!(&e, StoreError::HeldByReader(p) if *p == dir),
        "{e:?}"
    );
    // nothing was copied or appended: no backup beside the store, the store as it was
    assert!(
        names(&dir.parent().unwrap().to_path_buf())
            .iter()
            .all(|n| !n.starts_with(".store-upgrade-")),
        "no backup"
    );
    assert_eq!(tree(&dir), store_tree);
    assert_eq!(upgrade_batches(&dir), 0);
    assert_eq!(format_text(&dir), old);
    // released, the same plan applies; there is one backup, and no earlier copy blocks it
    drop(reader);
    let report = apply_with(&dir, &lock, &plan, &registry()).unwrap();
    assert!(report.changed && report.batch.is_some(), "{report:?}");
    assert_eq!(upgrade_batches(&dir), 1);
}
