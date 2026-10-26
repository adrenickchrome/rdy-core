//! ok12 (D-WWAK7, D-TXFHR, D-C8RZW): hosted store maintenance is an explicit call, never a side
//! effect of an open. A handle open requires a current store; the host runs `maintenance::upgrade`
//! with its handles closed and its captured readers released; a holder keeps its identity and
//! every acknowledged log byte through it. These tests use an injected step registry: the release
//! binary registers no step, so a nonempty plan needs the test seam.
use rdy_adapter::maintenance::{self, UpgradeStep};
use rdy_adapter::stream::ObjectKey;
use rdy_adapter::take::UploadTarget;
use rdy_adapter::{Hash, RdyError, RepoId, StoreHandle, StoreMode};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest, stream_export};
use rdy_model::Role;
use rdy_store::format::{
    FORMAT_FILE, FeatureClass, StoreAccess, StoreFeature, StoreFeatures, TEST_FEATURE,
    baseline_features, inject_test_feature, read_format_file,
};
use rdy_store::upgrade::{self, Phase};
use rdy_store::{OpenPolicy, Store, kinds};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const NEEDS_UPGRADE: &str = "the store needs an upgrade before it can open";
const NEEDS_UPGRADE_FIX: &str = "run the host's store maintenance, or rdy upgrade";
const LOCKED_FIX: &str = "drop --no-daemon, or stop the daemon with rdy serve --stop";

// ---------------------------------------------------------------------------------------------
// the injected registry

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

// ---------------------------------------------------------------------------------------------
// fixtures: a source project, and holders and projects built from it

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

/// Lands a doc titled `title` in `r`; returns the landing.
fn land_doc(r: &mut Rdy, title: &str) -> Hash {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "page"}}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r
        .propose(&session(Role::Writer), "work", title, Some("a summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap()
}

/// Tags canon's head `name`; returns the tag's hash.
fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

/// A closed source project at `dir` with two landings tagged `v1.0.0` and `v2.0.0`; returns the
/// repo id and the upload of each tag: its file, its target and its object keys.
struct Source {
    repo: RepoId,
    uploads: Vec<Upload>,
}

struct Upload {
    path: PathBuf,
    target: UploadTarget,
}

fn source(dir: &Path) -> Source {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let mut uploads = vec![];
    for (i, name) in ["v1.0.0", "v2.0.0"].into_iter().enumerate() {
        let landing = land_doc(&mut r, &format!("Doc {i}"));
        let t = tag(&mut r, name);
        let path = dir.join(format!("{name}.rdysync"));
        let mut out = Vec::new();
        stream_export::write(&r.read(), t, &BTreeSet::new(), &mut out).unwrap();
        std::fs::write(&path, out).unwrap();
        uploads.push(Upload {
            path,
            target: UploadTarget {
                tag: name.into(),
                landing,
            },
        });
    }
    let repo = r.store().repo_id().unwrap().unwrap();
    r.close().unwrap();
    Source { repo, uploads }
}

/// A closed holder at `root` that took the first `n` uploads of `src`.
fn holder(root: &Path, src: &Source, n: usize) {
    let mut h = StoreHandle::create_holder(root, src.repo).unwrap();
    for u in &src.uploads[..n] {
        h.take(&[File::open(&u.path).unwrap()], u.target.clone())
            .unwrap();
    }
    h.close().unwrap();
}

/// A closed project at `root` with one landed doc.
fn project(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let mut r = Rdy::init(root, "rdy-standard").unwrap();
    land_doc(&mut r, "Doc");
    r.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// store facts, read from the files

fn store_dir(root: &Path) -> PathBuf {
    root.join(".rdy").join("store")
}

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

/// The paths whose length or bytes differ between two trees (or exist in only one), as names.
fn changed(
    before: &BTreeMap<PathBuf, Option<(u64, Vec<u8>)>>,
    after: &BTreeMap<PathBuf, Option<(u64, Vec<u8>)>>,
) -> Vec<String> {
    let mut out = vec![];
    for p in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
        if before.get(p) != after.get(p) {
            out.push(p.display().to_string());
        }
    }
    out
}

/// The log's segment files, by name, with their bytes.
fn segments(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(store_dir(root).join("segments")).unwrap() {
        let p = e.unwrap().path();
        out.insert(
            p.file_name().unwrap().to_string_lossy().into_owned(),
            std::fs::read(&p).unwrap(),
        );
    }
    out
}

/// The `meta/upgrade` frames in the store's raw segments: one per upgrade batch.
fn upgrade_batches(root: &Path) -> usize {
    segments(root)
        .values()
        .map(|b| {
            b.windows(b"meta/upgrade".len())
                .filter(|w| *w == b"meta/upgrade")
                .count()
        })
        .sum()
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

fn format_text(root: &Path) -> String {
    std::fs::read_to_string(store_dir(root).join(FORMAT_FILE)).unwrap()
}

/// The stored frame (raw kind and payload) of each key, read from the closed store at `root`.
fn frames(root: &Path, keys: &[ObjectKey]) -> BTreeMap<ObjectKey, (u8, Vec<u8>)> {
    let s = Store::open(&store_dir(root)).unwrap();
    let r = s.reader();
    keys.iter()
        .map(|k| (*k, r.get_kind_raw(k.kind, &k.hash).unwrap()))
        .collect()
}

/// Leaves the upgrade of the store at `root` sealed and its completion pending: it stops after
/// its store check, before the engine ever runs.
fn make_pending(root: &Path) {
    let dir = store_dir(root);
    let stop = upgrade::with_hook_for_tests(
        |p| match p {
            Phase::Checked => Err(std::io::Error::other("stopped after the check")),
            _ => Ok(()),
        },
        || Store::open_with_registry_for_tests(&dir, OpenPolicy::AutoUpgrade, &registry()),
    );
    assert!(stop.is_err());
    let plan = upgrade::inspect(&dir).unwrap();
    assert!(plan.pending_check && plan.steps.is_empty(), "{plan:?}");
    assert_eq!(upgrade_batches(root), 1);
}

fn err<T>(r: Result<T, RdyError>) -> RdyError {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

fn assert_needs_upgrade(e: &RdyError) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        ("E_STORE_FORMAT", NEEDS_UPGRADE, NEEDS_UPGRADE_FIX),
        "{e:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn ok12_handle_open_never_runs_an_upgrade() {
    // a created holder starts with the current feature manifest (checked before any test
    // feature is registered)
    {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("h");
        StoreHandle::create_holder(&root, RepoId([5; 16]))
            .unwrap()
            .close()
            .unwrap();
        assert_eq!(format_text(&root), baseline_features().to_text());
        assert_eq!(
            read_format_file(&store_dir(&root)).unwrap(),
            Some(baseline_features())
        );
    }

    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let p = base.path().join("p");
    let h = base.path().join("h");
    project(&p);
    holder(&h, &source(&base.path().join("src")), 1);
    let before = tree(base.path());

    // planned steps: both opens refuse before any backup, append or mirror
    for (root, mode) in [(&p, StoreMode::Project), (&h, StoreMode::Holder)] {
        let e = err(StoreHandle::open_with_registry_for_tests(
            root,
            mode,
            &registry(),
        ));
        assert_needs_upgrade(&e);
        assert_eq!(
            changed(&before, &tree(base.path())),
            Vec::<String>::new(),
            "nothing was written"
        );
    }

    // this rdy's own registry plans nothing, so the same stores open, and read
    let hh = StoreHandle::open_holder(&h).unwrap();
    assert_eq!(hh.snapshot().unwrap().tags().unwrap().len(), 1);
    hh.close().unwrap();
    StoreHandle::open(&p).unwrap().close().unwrap();

    // a pending completion is refused too, by both production opens, with nothing written
    make_pending(&p);
    make_pending(&h);
    let before = tree(base.path());
    assert_needs_upgrade(&err(StoreHandle::open(&p)));
    assert_needs_upgrade(&err(StoreHandle::open_holder(&h)));
    assert_eq!(
        changed(&before, &tree(base.path())),
        Vec::<String>::new(),
        "nothing was written"
    );
    assert_eq!(format_text(&p), baseline_features().to_text());
    assert_eq!(upgrade_batches(&p), 1);
    assert_eq!(upgrade_batches(&h), 1);

    // the explicit call completes it with no second batch, and the opens then succeed
    for root in [&p, &h] {
        let plan = maintenance::inspect_store(root).unwrap();
        assert!(plan.pending_check && plan.steps.is_empty(), "{plan:?}");
        let report = maintenance::upgrade(root, false).unwrap();
        assert!(
            report.changed && report.checked && !report.pending,
            "{report:?}"
        );
        assert_eq!(report.to, upgraded());
        assert_eq!(format_text(root), upgraded().to_text());
        assert_eq!(upgrade_batches(root), 1);
    }
    StoreHandle::open(&p).unwrap().close().unwrap();
    StoreHandle::open_holder(&h).unwrap().close().unwrap();
}

#[test]
fn ok12_explicit_maintenance_preserves_holder_identity() {
    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("h");
    let src = source(&base.path().join("src"));
    holder(&root, &src, 2);

    // what the holder is, before maintenance
    let h = StoreHandle::open_holder(&root).unwrap();
    let snap = h.snapshot().unwrap();
    let identity = snap.identity().clone();
    assert_eq!(identity.repo_id, src.repo);
    let tags = snap.tags().unwrap();
    assert_eq!(tags.len(), 2);
    let canon = snap.canon();
    assert!(canon.is_none(), "a holder has no canon");
    let report = snap
        .write_stream("v2.0.0", &BTreeSet::new(), std::io::sink())
        .unwrap();
    let keys = report.keys.clone();
    assert!(!keys.is_empty());
    let fsck_before = serde_json::to_value(h.fsck().unwrap()).unwrap();
    drop(snap);
    h.close().unwrap();
    let frames_before = frames(&root, &keys);
    let log_before = segments(&root);

    // a dry run inspects: it creates no handle and writes nothing
    let all = tree(base.path());
    let plan = maintenance::inspect_store(&root).unwrap();
    assert!(
        plan.steps.is_empty(),
        "this rdy registers no step: {plan:?}"
    );
    let planned =
        rdy_engine::maintenance::plan_with_registry_for_tests(&root, &registry()).unwrap();
    assert_eq!(planned.steps, ["add_test"]);
    assert_eq!(
        (planned.from.clone(), planned.to.clone()),
        (baseline_features(), upgraded())
    );
    let dry = maintenance::upgrade_with_registry_for_tests(&root, true, &registry()).unwrap();
    assert!(dry.dry_run && !dry.changed && !dry.checked, "{dry:?}");
    assert_eq!(dry.steps, ["add_test"]);
    assert_eq!((dry.batch, dry.backup.clone()), (None, None));
    assert_eq!(
        changed(&all, &tree(base.path())),
        Vec::<String>::new(),
        "a dry run writes nothing"
    );

    // the apply: one upgrade batch, the full check, then the mirror
    let done = maintenance::upgrade_with_registry_for_tests(&root, false, &registry()).unwrap();
    assert!(
        done.changed && done.checked && !done.dry_run && !done.pending,
        "{done:?}"
    );
    assert_eq!(done.steps, ["add_test"]);
    assert_eq!(
        (done.from.clone(), done.to.clone()),
        (baseline_features(), upgraded())
    );
    assert!(done.batch.is_some());
    assert!(done.backup.as_ref().is_some_and(|b| b.exists()), "{done:?}");
    assert_eq!(format_text(&root), upgraded().to_text());
    assert_eq!(upgrade_batches(&root), 1);

    // acknowledged log bytes are kept: each old segment is a prefix of the new one
    let log_after = segments(&root);
    for (name, old) in &log_before {
        let new = &log_after[name];
        assert!(new.len() > old.len() && new.starts_with(old), "{name}");
    }

    // the same holder reopens with its identity, tags, objects and a clean check
    let h = StoreHandle::open_holder(&root).unwrap();
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.identity(), &identity);
    assert_eq!(snap.tags().unwrap(), tags);
    assert_eq!(snap.canon(), canon);
    let again = snap
        .write_stream("v2.0.0", &BTreeSet::new(), std::io::sink())
        .unwrap();
    assert_eq!(again.keys, keys);
    assert_eq!(again.header.repo_id, src.repo);
    drop(snap);
    // the check counts the same records; only the upgrade batch's frames are new
    let mut fsck_after = serde_json::to_value(h.fsck().unwrap()).unwrap();
    let (frames_after, frames_before_n) = (
        fsck_after["frames"].as_u64().unwrap(),
        fsck_before["frames"].as_u64().unwrap(),
    );
    assert!(frames_after > frames_before_n, "{fsck_after} {fsck_before}");
    fsck_after["frames"] = fsck_before["frames"].clone();
    assert_eq!(fsck_after, fsck_before);
    h.close().unwrap();
    assert_eq!(frames(&root, &keys), frames_before, "source bytes are kept");

    // a second call has nothing to do, and the plan is empty
    let plan = maintenance::inspect_store(&root).unwrap();
    assert!(plan.steps.is_empty() && !plan.pending_check, "{plan:?}");
    let idle = maintenance::upgrade_with_registry_for_tests(&root, false, &registry()).unwrap();
    assert!(!idle.changed && idle.steps.is_empty(), "{idle:?}");
    assert_eq!(upgrade_batches(&root), 1);
}

#[test]
fn ok12_held_snapshot_blocks_maintenance_until_released() {
    let _f = known();
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("h");
    holder(&root, &source(&base.path().join("src")), 1);
    let locked = |root: &Path| {
        err(maintenance::upgrade_with_registry_for_tests(
            root,
            false,
            &registry(),
        ))
    };

    // a running owner: the handle holds the store's lock
    let h = StoreHandle::open_holder(&root).unwrap();
    let e = locked(&root);
    assert_eq!((e.code, e.fix.as_str()), ("E_LOCKED", LOCKED_FIX), "{e:?}");
    assert_eq!(
        e.message,
        format!(
            "the project store at {} is held by another rdy process (the daemon, or a command run with --no-daemon)",
            root.join(".rdy").join("store").display()
        )
    );
    // a dry run needs neither the lock nor a handle, and lists the plan
    let dry = maintenance::upgrade_with_registry_for_tests(&root, true, &registry()).unwrap();
    assert_eq!(dry.steps, ["add_test"]);
    h.close().unwrap();

    // an outstanding reader: another thread keeps a captured snapshot of a dropped handle until
    // told to release it. The barriers fix the order: ready, attempted, released.
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let (attempted_tx, attempted_rx) = mpsc::channel::<()>();
    let (released_tx, released_rx) = mpsc::channel::<()>();
    let reader_root = root.clone();
    let reader = std::thread::spawn(move || {
        let h = StoreHandle::open_holder(&reader_root).unwrap();
        let snap = h.snapshot().unwrap();
        drop(h);
        let mut first = Vec::new();
        snap.write_stream("v1.0.0", &BTreeSet::new(), &mut first)
            .unwrap();
        ready_tx.send(()).unwrap();
        attempted_rx.recv().unwrap();
        // the snapshot still answers what it captured, byte for byte
        let mut second = Vec::new();
        snap.write_stream("v1.0.0", &BTreeSet::new(), &mut second)
            .unwrap();
        assert_eq!(first, second);
        drop(snap);
        released_tx.send(()).unwrap();
    });
    ready_rx.recv().unwrap();
    let before = tree(&store_dir(&root));
    let e = locked(&root);
    assert_eq!(e.code, "E_HELD_BY_READER", "{e:?}");
    assert_eq!(
        e.message,
        format!(
            "the project store at {} is still open in this process through a read state of an earlier open",
            store_dir(&root).display()
        ),
        "{e:?}"
    );
    assert_eq!(e.fix, "release the earlier read state, then try again");
    // the store is as it was: no backup, no upgrade batch, no new mirror
    assert_eq!(
        changed(&before, &tree(&store_dir(&root))),
        Vec::<String>::new(),
        "nothing was written"
    );
    assert_eq!(upgrade_batches(&root), 0);
    assert_eq!(format_text(&root), baseline_features().to_text());
    assert!(
        backups(&root).is_empty(),
        "the refusal came before the backup"
    );
    attempted_tx.send(()).unwrap();
    released_rx.recv().unwrap();
    reader.join().unwrap();

    // released: the call runs
    let done = maintenance::upgrade_with_registry_for_tests(&root, false, &registry()).unwrap();
    assert!(done.changed && done.checked, "{done:?}");
    assert_eq!(format_text(&root), upgraded().to_text());
    assert_eq!(upgrade_batches(&root), 1);
    assert_eq!(backups(&root).len(), 1);
}

#[test]
fn ok12_read_only_holder_exports_but_cannot_take() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("h");
    let src = source(&base.path().join("src"));
    // the holder's first take activates a read-only feature this rdy knows only while the
    // guard is held
    let keys = {
        let _ro = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::TAG);
        holder(&root, &src, 1);
        assert!(
            read_format_file(&store_dir(&root))
                .unwrap()
                .unwrap()
                .has(TEST_FEATURE)
        );
        let h = StoreHandle::open_holder(&root).unwrap();
        let keys = h
            .snapshot()
            .unwrap()
            .write_stream("v1.0.0", &BTreeSet::new(), std::io::sink())
            .unwrap()
            .keys;
        h.close().unwrap();
        keys
    };
    // the guard is gone: the feature is unknown, and the store opens for reads only
    let rdy = Rdy::open_holder(&root).unwrap();
    assert_eq!(rdy.store().access(), StoreAccess::ReadOnly);
    rdy.close().unwrap();
    let before = tree(base.path());

    let mut h = StoreHandle::open_holder(&root).unwrap();
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.tags().unwrap().len(), 1);
    let mut exported = Vec::new();
    let report = snap
        .write_stream("v1.0.0", &BTreeSet::new(), &mut exported)
        .unwrap();
    assert_eq!(report.keys, keys);
    drop(snap);
    h.dump(&base.path().join("ro.rdydump")).unwrap();
    h.fsck().unwrap();

    // a take of the second upload fails through the store gate, with nothing appended
    let second = &src.uploads[1];
    let file = File::open(&second.path).unwrap();
    let e = err(h.take(&[file], second.target.clone()));
    let read_only = (
        "E_STORE_READ_ONLY",
        "store feature test_feature permits reads only",
        "open the project with rdy 99.0.0 or later to write",
    );
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        read_only,
        "{e:?}"
    );
    let file = File::open(&second.path).unwrap();
    let target = rdy_adapter::PublicationTarget {
        upload: second.target.clone(),
        profile: "site".into(),
    };
    match h.take_publication(&[file], target, |_| Ok::<(), ()>(())) {
        Err(rdy_adapter::PublicationRefusal::Core(e)) => {
            assert_eq!(
                (e.code, e.message.as_str(), e.fix.as_str()),
                read_only,
                "{e:?}"
            );
        }
        other => panic!("expected the store gate, got {:?}", other.map(|_| ())),
    }
    h.close().unwrap();
    // the dump file is the only new path; the store is byte for byte as it was
    let mut after = tree(base.path());
    assert!(after.remove(&base.path().join("ro.rdydump")).is_some());
    assert_eq!(after, before, "the read-only handle wrote nothing");

    // maintenance has nothing to do for it and writes nothing
    let report = maintenance::upgrade(&root, false).unwrap();
    assert!(!report.changed && report.steps.is_empty(), "{report:?}");
    assert_eq!(
        changed(&before, &tree(base.path())).len(),
        1,
        "only the dump is new"
    );
}

#[test]
fn ok12_format_facts_are_shared_with_core() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("h");
    holder(&root, &source(&base.path().join("src")), 1);

    // the format facts are core's, the same values and the same types
    let facts: rdy_engine::formats::FormatFacts = maintenance::formats();
    assert_eq!(facts, rdy_engine::formats::formats());
    let features: &rdy_store::format::StoreFeatures = &facts.store;
    assert_eq!(features, &baseline_features());
    assert_eq!(facts.reads_features, [1]);

    // a dump's facts are core's, from its prelude and header only
    let h = StoreHandle::open_holder(&root).unwrap();
    let dump = base.path().join("h.rdydump");
    h.dump(&dump).unwrap();
    h.close().unwrap();
    let seen = maintenance::inspect_dump(&dump).unwrap();
    assert_eq!(seen, rdy_engine::formats::inspect_dump(&dump).unwrap());
    assert_eq!(
        seen.store,
        maintenance::DumpStore::Features(baseline_features())
    );

    // the plan and the report are core's helpers' values: one shape, one JSON
    let plan: maintenance::UpgradePlan = maintenance::inspect_store(&root).unwrap();
    let core_plan = rdy_engine::maintenance::plan(&root).unwrap();
    assert_eq!(plan, core_plan);
    assert!(plan.steps.is_empty() && !plan.requires_upgrade && !plan.pending_check);
    assert_eq!(plan.from, baseline_features());
    for dry_run in [true, false] {
        let report: maintenance::UpgradeReport = maintenance::upgrade(&root, dry_run).unwrap();
        let core = rdy_engine::maintenance::run(&root, dry_run).unwrap();
        assert_eq!(report, core);
        assert_eq!(
            rdy_engine::maintenance::report_json(&report),
            rdy_engine::maintenance::report_json(&core)
        );
        assert!(!report.changed && report.steps.is_empty(), "{report:?}");
    }

    // the same with a nonempty plan
    let _f = known();
    let adapter = maintenance::upgrade_with_registry_for_tests(&root, true, &registry()).unwrap();
    let core =
        rdy_engine::maintenance::run_with_registry_for_tests(&root, true, &registry()).unwrap();
    assert_eq!(adapter, core);
    assert_eq!(adapter.steps, ["add_test"]);
}
