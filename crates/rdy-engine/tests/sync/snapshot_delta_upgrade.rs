//! sdl5 (D-918YB, D-8T7PA): the engine's paths of the object-producing `snapshot_delta` step.
//! Readers (an adapter's `RequireCurrent` open, a follower) read a legacy store as it is; the
//! maintenance runs the step, an empty store included; a load of a dump without the feature
//! appends one tail batch with the deltas and the manifest, and a load or follower of a chain
//! keeps the source's prefix; and a take converts a legacy upload into an upgraded holder in
//! its own batch, while a holder without the feature refuses an upload that has it. The step
//! is passed in as a test registry: production registers none until Task 6.
use crate::common;

use rdy_engine::dump_stop::DumpStop;
use rdy_engine::follower::FollowerCore;
use rdy_engine::publication_take::{PublicationRefusal, PublicationTarget, take_publication};
use rdy_engine::*;
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader};
use rdy_store::format::{FEATURE_FIGURE_DARK, FEATURE_SNAPSHOT_DELTA, FEATURES_FILE};
use rdy_store::holder::StoreMode;
use rdy_store::segment::Location;
use rdy_store::sync_stream::StreamHeader;
use rdy_store::take_batch::{TakeError, UploadTarget};
use rdy_store::upgrade::{self, SNAPSHOT_DELTA_STEP, UpgradeStep};
use rdy_store::{OpenPolicy, Store, StoreError, codec, kinds};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

fn steps() -> Vec<UpgradeStep> {
    vec![upgrade::snapshot_delta_step()]
}

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("sdl5 {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e}"))
}

fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&writer(), view, "sdl5", Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap().parse().unwrap()
}

/// A project at `<base>/src` with `n` landings on canon (a doc, then one prose block per
/// landing), tagged `v1.0.0` at the last one.
fn project(base: &Path, n: usize) -> (PathBuf, Rdy, Vec<Hash>) {
    let root = base.join("src");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "Prose 0."}
        ]),
    );
    let doc = w.created["$d"].clone();
    let mut landings = vec![land(&mut r, "work")];
    for i in 1..n {
        write(
            &mut r,
            "work",
            json!([{"op": "create", "type": "prose", "id": "$p", "parent": doc, "body": format!("Prose {i}.")}]),
        );
        landings.push(land(&mut r, "work"));
    }
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    (root, r, landings)
}

fn tag_hash(r: &Rdy, name: &str) -> Hash {
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

/// Appends each canon landing's delta, oldest first, from its held SNAPSHOT: the source then
/// holds the feature, as an upgraded one does.
fn add_deltas(r: &mut Rdy) {
    let mut chain = r.store().landings().unwrap();
    chain.reverse();
    let mut items = vec![];
    let mut prev: Option<(BTreeMap<BlockId, Hash>, Hash)> = None;
    for (landing, rec) in chain {
        let map = r.store().get::<StateSnapshot>(&rec.state).unwrap().blocks;
        let empty = BTreeMap::new();
        let (before, base) = match &prev {
            Some((m, d)) => (m, Some(*d)),
            None => (&empty, None),
        };
        let d = diff_snapshots(landing, base, before, &map);
        items.push(codec::item_new(kinds::SNAPSHOT_DELTA, &d));
        prev = Some((map, d.id()));
    }
    r.store_mut().write(items).unwrap();
    r.invalidate();
}

/// One batch file of tag `tag`'s whole closure in project `r`, with its store's features.
fn export(r: &Rdy, tag: Hash, path: &Path) {
    let reader = r.store().reader();
    let c = closure(&reader, tag, &BTreeSet::new()).unwrap();
    let header = StreamHeader::tag_history(
        reader.store_features().unwrap(),
        r.store().repo_id().unwrap().unwrap(),
        tag,
        c.prerequisites,
    );
    let mut source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    write_batch(
        &mut source,
        &header,
        &c.records,
        File::create(path).unwrap(),
    )
    .unwrap();
}

fn target(r: &Rdy, tag: Hash) -> PublicationTarget {
    let t = r.store().read_tag(&tag).unwrap();
    PublicationTarget {
        upload: UploadTarget {
            tag: t.name,
            landing: t.landing,
        },
        profile: "implicit:org".into(),
    }
}

fn publish(h: &mut Rdy, file: &Path, target: PublicationTarget) -> Result<(), RdyError> {
    take_publication(h, &[File::open(file).unwrap()], target, |_| Ok::<_, ()>(()))
        .map(|_| ())
        .map_err(|e| match e {
            PublicationRefusal::Core(e) => e,
            PublicationRefusal::Verdict(()) => panic!("the verdict refused"),
        })
}

/// A holder of `repo` at `root`; with `upgraded`, the maintenance has run the step on it while
/// it was empty. It is opened as it is: a writer's open with the step would run it.
fn holder(root: &Path, repo: RepoId, upgraded: bool) -> Rdy {
    Rdy::create_holder(root, repo).unwrap().close().unwrap();
    if upgraded {
        let report = maintenance::run_with_registry_for_tests(root, false, &steps()).unwrap();
        assert!(report.changed && report.checked);
    }
    Rdy::open_holder_with_policy(root, OpenPolicy::RequireCurrent).unwrap()
}

fn store_dir(root: &Path) -> PathBuf {
    root.join(".rdy").join("store")
}

/// The store under `root` as it is: never upgraded by the open, a follower's included.
fn open_as_is(root: &Path) -> Store {
    let dir = store_dir(root);
    match Store::open_with_policy(&dir, OpenPolicy::RequireCurrent) {
        Err(StoreError::Follower(_)) => Store::open_follower(&dir).unwrap(),
        // a sealed upgrade whose completion is pending: the upgrade's own open
        Err(StoreError::UpgradeRequired) => {
            let lock = rdy_store::StoreLock::take(&dir).unwrap();
            upgrade::open_pending_for_tests(&dir, &lock).unwrap()
        }
        s => s.unwrap(),
    }
}

/// The sealed batches of the store under `root` in `[from, to)`.
fn batches(root: &Path, from: Location, to: Option<Location>) -> Vec<DumpBatch> {
    let s = open_as_is(root);
    let to = to.unwrap_or_else(|| s.log_end());
    let mut out = vec![];
    s.reader()
        .for_each_batch_after(from, to, &mut |b| {
            out.push(b);
            Ok(())
        })
        .unwrap();
    out
}

/// The batches of the dump `file`, and its header's log end.
fn dump_batches(file: &Path) -> (Location, Vec<DumpBatch>) {
    let (mut r, _, h) = DumpReader::open(BufReader::new(File::open(file).unwrap())).unwrap();
    let mut out = vec![];
    while let Some(item) = r.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            out.push(b);
        }
    }
    (h.log_end, out)
}

/// The raw kinds of a batch's frames, in order.
fn kinds_of(b: &DumpBatch) -> Vec<u8> {
    b.frames.iter().map(|f| f.kind()).collect()
}

fn is_delta(kind: u8) -> bool {
    codec::logical_kind(kind) == kinds::SNAPSHOT_DELTA
}

/// The SNAPSHOT_DELTA frames the store under `root` holds.
fn delta_count(root: &Path) -> usize {
    batches(root, ORIGIN, None)
        .iter()
        .flat_map(kinds_of)
        .filter(|k| is_delta(*k))
        .count()
}

/// The upgrade backups beside the store under `root`.
fn backups(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join(".rdy"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(upgrade::BACKUP_PREFIX))
        .collect();
    out.sort();
    out
}

fn upgrade_batches(root: &Path) -> usize {
    batches(root, ORIGIN, None)
        .iter()
        .flat_map(|b| b.frames.iter())
        .filter(|f| {
            f.kind() == kinds::REF_UPDATE
                && postcard::from_bytes::<kinds::RefUpdate>(f.present_payload().unwrap())
                    .is_ok_and(|u| u.name == rdy_store::format::REF_UPGRADE)
        })
        .count()
}

/// Every landing of the store under `root` reads the map its SNAPSHOT holds, through its delta.
fn reads_agree(root: &Path) {
    let s = open_as_is(root);
    let r = s.reader();
    for (l, rec) in r.project_landings().unwrap() {
        assert!(s.snapshot_delta_of(&l).unwrap().is_some(), "{l}: a delta");
        assert_eq!(r.snapshot_at(&l).unwrap().id(), rec.state, "{l}");
    }
}

fn triple(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

const UPGRADE_REQUIRED: (&str, &str, &str) = (
    E_STORE_FORMAT,
    "the store needs an upgrade before it can open",
    "run the host's store maintenance, or rdy upgrade",
);

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn sdl5_optional_step_keeps_adapter_and_follower_reads() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = rdy_store::format::legacy_writer_for_tests();
    let base = tempfile::tempdir().unwrap();
    let (root, r, landings) = project(base.path(), 2);
    let dump = base.path().join("p.rdydump");
    r.read().dump(&dump).unwrap();
    r.close().unwrap();
    let segs = |root: &Path| batches(root, ORIGIN, None);
    let before = segs(&root);
    upgrade::with_registry_for_tests(&steps(), || {
        // the adapter's open: a legacy store opens as it is, with no backup, batch or walk
        let walks = upgrade::sealed_walks_for_tests();
        let l = Local::open_with_policy(&root, OpenPolicy::RequireCurrent).unwrap();
        drop(l);
        assert_eq!(upgrade::sealed_walks_for_tests(), walks);
        let r = Rdy::open_with_policy(&root, OpenPolicy::RequireCurrent).unwrap();
        assert!(!r.store().features().has(FEATURE_SNAPSHOT_DELTA));
        assert_eq!(r.store().landings().unwrap().len(), 2);
        r.close().unwrap();
        // a test registry passed in plans it, and still opens it as it is
        let r = Rdy::open_with_registry_for_tests(
            &root,
            StoreMode::Project,
            OpenPolicy::RequireCurrent,
            &steps(),
        )
        .unwrap();
        r.close().unwrap();
        // a follower seeds from the dump as it is, and its reads work
        let f = base.path().join("f");
        rdy_engine::follower::seed(&dump, &f).unwrap();
        let g = base.path().join("g");
        rdy_engine::follower::seed_with_registry_for_tests(&dump, &g, &steps()).unwrap();
        for dir in [&f, &g] {
            let core = FollowerCore::open(dir).unwrap();
            let state = core.read().unwrap();
            assert_eq!(state.landings().unwrap()[0].0, landings[1]);
            drop(state);
            core.fsck().unwrap();
            drop(core);
            let s = Store::open_follower(&store_dir(dir)).unwrap();
            assert!(!s.features().has(FEATURE_SNAPSHOT_DELTA));
        }
    });
    assert_eq!(segs(&root), before, "no byte appended");
    assert!(backups(&root).is_empty());
    assert_eq!(delta_count(&root), 0);
}

#[test]
fn sdl5_empty_store_upgrade_activates_without_deltas() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = rdy_store::format::legacy_writer_for_tests();
    for interrupted in [false, true] {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("p");
        let r = Rdy::init(&root, "rdy-standard").unwrap();
        assert!(r.store().landings().unwrap().is_empty());
        let from = r.store().features().clone();
        r.close().unwrap();
        let end = batches(&root, ORIGIN, None).len();
        let dry = maintenance::run_with_registry_for_tests(&root, true, &steps()).unwrap();
        assert_eq!(dry.steps, vec![SNAPSHOT_DELTA_STEP.to_string()]);
        assert!(!dry.changed);
        let report = if interrupted {
            // the completion stops before its fsck: the batch is durable and pending, and the
            // next writer's open finishes it with no second batch or backup
            let e = with_completion_hook_for_tests(
                |p| match p {
                    CompletionPhase::BeforeFsck => {
                        Err(RdyError::new(E_IO, "stopped here", "retry"))
                    }
                    _ => Ok(()),
                },
                || maintenance::run_with_registry_for_tests(&root, false, &steps()),
            )
            .unwrap_err();
            assert_eq!(e.message, "stopped here");
            assert_eq!(upgrade_batches(&root), 1);
            assert_eq!(
                std::fs::read_to_string(store_dir(&root).join(FEATURES_FILE)).unwrap(),
                from.to_text(),
                "the mirror waits for the completion"
            );
            let r = Rdy::open_with_registry_for_tests(
                &root,
                StoreMode::Project,
                OpenPolicy::AutoUpgrade,
                &steps(),
            )
            .unwrap();
            r.close().unwrap();
            maintenance::run_with_registry_for_tests(&root, false, &steps()).unwrap()
        } else {
            let report = maintenance::run_with_registry_for_tests(&root, false, &steps()).unwrap();
            assert!(report.changed && report.checked && report.reindexed);
            assert_eq!(report.steps, vec![SNAPSHOT_DELTA_STEP.to_string()]);
            let receipt = report.receipt.clone().unwrap();
            assert_eq!(receipt.from, from.hash());
            assert_eq!(receipt.to, report.to.hash());
            assert_eq!(receipt.steps, vec![SNAPSHOT_DELTA_STEP.to_string()]);
            assert_eq!(backups(&root), vec![receipt.backup.clone()]);
            report
        };
        // one batch: no delta and no landing, only the features and receipt refs
        let all = batches(&root, ORIGIN, None);
        assert_eq!(all.len(), end + 1, "interrupted {interrupted}");
        assert_eq!(
            kinds_of(&all[end]),
            vec![kinds::REF_UPDATE, kinds::REF_UPDATE],
            "interrupted {interrupted}"
        );
        assert_eq!(delta_count(&root), 0);
        let to = from.merged(&[rdy_store::format::StoreFeature {
            name: FEATURE_SNAPSHOT_DELTA.into(),
            class: rdy_store::format::FeatureClass::Incompatible,
            required_rdy: rdy_store::format::SNAPSHOT_DELTA_REQUIRED_RDY.into(),
        }]);
        assert_eq!(
            std::fs::read_to_string(store_dir(&root).join(FEATURES_FILE)).unwrap(),
            to.to_text()
        );
        // a second run adds nothing
        let again = maintenance::run_with_registry_for_tests(&root, false, &steps()).unwrap();
        assert!(!again.changed && again.batch.is_none(), "{report:?}");
        assert_eq!(upgrade_batches(&root), 1);
        assert_eq!(backups(&root).len(), 1);
        let r = Rdy::open(&root).unwrap();
        assert!(r.store().features().has(FEATURE_SNAPSHOT_DELTA));
        r.fsck().unwrap();
    }
}

#[test]
fn sdl5_old_dump_load_appends_one_combined_batch() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = rdy_store::format::legacy_writer_for_tests();
    let d = tempfile::tempdir().unwrap();
    for name in [
        "v1-s10",
        "v2-s13",
        "v2-s15-holder",
        "v4-f1-e1",
        "v4-f1-e1-figure-dark",
    ] {
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../rdy-qa/fixtures/dump/{name}.rdydump"));
        let (log_end, source) = dump_batches(&file);
        let to = d.path().join(name);
        let s = upgrade::with_registry_for_tests(&steps(), || load::load(&file, &to))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(s.batches, source.len() as u64, "{name}");
        // the source batches with their hashes, then one tail: the deltas, then the refs
        assert_eq!(batches(&to, ORIGIN, Some(log_end)), source, "{name}");
        let tail = batches(&to, log_end, None);
        assert_eq!(tail.len(), 1, "{name}: {tail:?}");
        let k = kinds_of(&tail[0]);
        let deltas = k.iter().take_while(|k| is_delta(**k)).count();
        assert!(
            k[deltas..].iter().all(|k| *k == kinds::REF_UPDATE),
            "{name}: {k:?}"
        );
        let landings = source
            .iter()
            .flat_map(kinds_of)
            .filter(|k| codec::logical_kind(*k) == kinds::LANDING)
            .count();
        assert!(landings > 0, "{name}: the fixture lands");
        assert_eq!(deltas, landings, "{name}: one delta per landing");
        // the dump is the backup: none is made, and no receipt is written
        assert!(backups(&to).is_empty(), "{name}");
        assert_eq!(upgrade_batches(&to), 0, "{name}");
        let st = Store::open(&store_dir(&to)).unwrap();
        let features = st.features().clone();
        assert!(features.has(FEATURE_SNAPSHOT_DELTA), "{name}");
        if name.ends_with("figure-dark") {
            let fd = features
                .features
                .iter()
                .find(|f| f.name == FEATURE_FIGURE_DARK)
                .unwrap_or_else(|| panic!("{name}: figure_dark kept"));
            assert_eq!(fd.required_rdy, rdy_store::format::FIGURE_DARK_REQUIRED_RDY);
        }
        drop(st);
        assert_eq!(
            std::fs::read_to_string(store_dir(&to).join(FEATURES_FILE)).unwrap(),
            features.to_text(),
            "{name}"
        );
        reads_agree(&to);
        // a load of that store's own dump holds the feature: it appends no conversion batch
        let again = d.path().join(format!("{name}.again.rdydump"));
        let src = if to.join(".rdy.yaml").exists() {
            Rdy::open(&to).unwrap()
        } else {
            Rdy::open_holder(&to).unwrap()
        };
        src.read().dump(&again).unwrap();
        src.close().unwrap();
        let (end2, _) = dump_batches(&again);
        let to2 = d.path().join(format!("{name}-2"));
        upgrade::with_registry_for_tests(&steps(), || load::load(&again, &to2)).unwrap();
        assert!(batches(&to2, end2, None).is_empty(), "{name}: no tail");
        assert_eq!(delta_count(&to2), landings, "{name}");
    }
}

#[test]
fn sdl5_empty_dump_load_activates_in_one_batch_without_deltas() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = rdy_store::format::legacy_writer_for_tests();
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    let r = Rdy::init(&root, "rdy-standard").unwrap();
    let dump = base.path().join("empty.rdydump");
    r.read().dump(&dump).unwrap();
    r.close().unwrap();
    let (log_end, source) = dump_batches(&dump);
    let to = base.path().join("loaded");
    let s = upgrade::with_registry_for_tests(&steps(), || load::load(&dump, &to)).unwrap();
    assert_eq!(s.landings, 0);
    assert_eq!(batches(&to, ORIGIN, Some(log_end)), source);
    let tail = batches(&to, log_end, None);
    assert_eq!(tail.len(), 1, "{tail:?}");
    assert_eq!(
        kinds_of(&tail[0]),
        vec![kinds::REF_UPDATE],
        "no landing or delta"
    );
    assert_eq!(delta_count(&to), 0);
    assert!(backups(&to).is_empty());
    let r = Rdy::open(&to).unwrap();
    assert!(r.store().features().has(FEATURE_SNAPSHOT_DELTA));
    assert!(r.store().landings().unwrap().is_empty());
    r.fsck().unwrap();
}

#[test]
fn sdl5_chain_stop_and_follower_seed_keep_the_source_prefix() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = rdy_store::format::legacy_writer_for_tests();
    let base = tempfile::tempdir().unwrap();
    let (root, r, _) = project(base.path(), 2);
    let f0 = base.path().join("f0.rdydump");
    r.read().dump(&f0).unwrap();
    r.close().unwrap();
    // the source upgrades: its next increment carries its own upgrade batch
    let report = maintenance::run_with_registry_for_tests(&root, false, &steps()).unwrap();
    assert!(report.changed);
    let f1 = base.path().join("f1.rdydump");
    let r = Rdy::open(&root).unwrap();
    r.read().dump_after_file(&f0, &f1).unwrap();
    r.close().unwrap();
    let (end0, _) = dump_batches(&f0);
    let (end1, _) = dump_batches(&f1);
    let source = batches(&root, ORIGIN, None);
    let landings = 2;
    upgrade::with_registry_for_tests(&steps(), || {
        // stopped before the source's upgrade: the prefix is legacy, and the load converts it
        // in its own one tail
        let a = base.path().join("a");
        let stop = DumpStop::Position {
            segment: end0.segment,
            offset: end0.offset,
        };
        load::load_chain(&[f0.clone(), f1.clone()], &a, Some(&stop)).unwrap();
        let prefix = batches(&a, ORIGIN, Some(end0));
        assert_eq!(prefix[..], source[..prefix.len()]);
        let tail = batches(&a, end0, None);
        assert_eq!(tail.len(), 1, "{tail:?}");
        assert_eq!(
            kinds_of(&tail[0]).iter().filter(|k| is_delta(**k)).count(),
            landings
        );
        reads_agree(&a);
        // through the source's upgrade: its batch is kept as it is, and nothing follows it
        let b = base.path().join("b");
        load::load_chain(&[f0.clone(), f1.clone()], &b, None).unwrap();
        assert_eq!(batches(&b, ORIGIN, Some(end1)), source);
        assert!(batches(&b, end1, None).is_empty());
        assert_eq!(delta_count(&b), landings);
        reads_agree(&b);
        // a follower keeps the source's prefix: no optional upgrade of its own
        let f = base.path().join("f");
        rdy_engine::follower::seed(&f0, &f).unwrap();
        assert_eq!(
            batches(&f, ORIGIN, None)[..],
            source[..batches(&f, ORIGIN, None).len()]
        );
        assert_eq!(delta_count(&f), 0);
        let mut core = FollowerCore::open(&f).unwrap();
        core.follow(&f1).unwrap();
        core.fsck().unwrap();
        drop(core);
        assert_eq!(
            batches(&f, ORIGIN, None),
            source,
            "the source's log, as it is"
        );
    });
}

#[test]
fn sdl5_take_feature_matrix_is_atomic() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = rdy_store::format::legacy_writer_for_tests();
    let base = tempfile::tempdir().unwrap();
    // a legacy source and a source with deltas, each tagged
    let (_, legacy, _) = project(&base.path().join("l"), 2);
    let (_, mut fed, _) = project(&base.path().join("f"), 2);
    add_deltas(&mut fed);
    let lt = tag_hash(&legacy, "v1.0.0");
    let ft = tag_hash(&fed, "v1.0.0");
    let lfile = base.path().join("legacy.rdysync");
    let ffile = base.path().join("fed.rdysync");
    export(&legacy, lt, &lfile);
    export(&fed, ft, &ffile);
    let lrepo = legacy.store().repo_id().unwrap().unwrap();
    let frepo = fed.store().repo_id().unwrap().unwrap();
    upgrade::with_registry_for_tests(&steps(), || {
        // no feature into no feature: the legacy take, unchanged
        let mut h = holder(&base.path().join("h1"), lrepo, false);
        publish(&mut h, &lfile, target(&legacy, lt)).unwrap();
        assert!(!h.store().features().has(FEATURE_SNAPSHOT_DELTA));
        h.store().check_holder().unwrap();
        let root = base.path().join("h1");
        h.close().unwrap();
        assert_eq!(delta_count(&root), 0);

        // no feature into an upgraded holder: the take writes a delta of each new landing in its
        // own one batch, beside the SNAPSHOT records it received
        let root = base.path().join("h2");
        let mut h = holder(&root, lrepo, true);
        let end = h.store().log_end();
        publish(&mut h, &lfile, target(&legacy, lt)).unwrap();
        h.store().check_holder().unwrap();
        h.close().unwrap();
        let after = batches(&root, end, None);
        let new: Vec<&DumpBatch> = after
            .iter()
            .filter(|b| {
                kinds_of(b)
                    .iter()
                    .any(|k| codec::logical_kind(*k) == kinds::LANDING)
            })
            .collect();
        assert_eq!(new.len(), 1, "one take batch");
        let k = kinds_of(new[0]);
        assert_eq!(k.iter().filter(|k| is_delta(**k)).count(), 2);
        assert_eq!(k.iter().filter(|k| **k == kinds::SNAPSHOT).count(), 2);
        reads_agree(&root);
        // and a refusal of that batch (here the holder's capacity, counted with the deltas it
        // would make) keeps the holder as it was
        let root = base.path().join("h2b");
        let mut h = holder(&root, lrepo, true);
        let end = h.store().log_end();
        rdy_store::take_batch::set_holder_capacity_for_tests(Some(
            rdy_store::take_batch::HolderCapacity {
                landing_depth: 2,
                objects: 1_000,
                edges: 1,
            },
        ));
        let refused = publish(&mut h, &lfile, target(&legacy, lt));
        rdy_store::take_batch::set_holder_capacity_for_tests(None);
        assert_eq!(refused.unwrap_err().code, E_LIMIT);
        h.close().unwrap();
        let h = Rdy::open_holder_with_policy(&root, OpenPolicy::RequireCurrent).unwrap();
        assert_eq!(h.store().log_end(), end, "nothing acknowledged");
        assert!(h.store().landings().unwrap().is_empty());
        h.close().unwrap();

        // the feature into a legacy holder: the existing upgrade-required refusal, unwritten
        let mut h = holder(&base.path().join("h3"), frepo, false);
        let end = h.store().log_end();
        let e = publish(&mut h, &ffile, target(&fed, ft)).unwrap_err();
        assert_eq!(triple(&e), UPGRADE_REQUIRED);
        assert_eq!(h.store().log_end(), end);
        assert!(!h.store().features().has(FEATURE_SNAPSHOT_DELTA));
        h.close().unwrap();

        // the feature into an upgraded holder: the supplied deltas are taken as they are
        let root = base.path().join("h4");
        let mut h = holder(&root, frepo, true);
        publish(&mut h, &ffile, target(&fed, ft)).unwrap();
        h.store().check_holder().unwrap();
        h.close().unwrap();
        assert_eq!(delta_count(&root), 2);
        reads_agree(&root);
    });
    // a delta the upload lacks is never made up: the last landing's is left out of its file
    let f2 = base.path().join("fed2.rdysync");
    {
        let reader = fed.store().reader();
        let c = closure(&reader, ft, &BTreeSet::new()).unwrap();
        let last = fed.store().read_tag(&ft).unwrap().landing;
        let (left_out, _) = fed.store().snapshot_delta_of(&last).unwrap().unwrap();
        let records: Vec<_> = c
            .records
            .iter()
            .copied()
            .filter(|k| k.hash != left_out)
            .collect();
        assert_eq!(records.len() + 1, c.records.len());
        let header =
            StreamHeader::tag_history(reader.store_features().unwrap(), frepo, ft, c.prerequisites);
        let mut source = StoreSource {
            reader: &reader,
            end: END_OF_LOG,
        };
        write_batch(&mut source, &header, &records, File::create(&f2).unwrap()).unwrap();
    }
    upgrade::with_registry_for_tests(&steps(), || {
        let root = base.path().join("h5");
        let mut h = holder(&root, frepo, true);
        let end = h.store().log_end();
        let e = publish(&mut h, &f2, target(&fed, ft)).unwrap_err();
        assert!(
            e.message.contains("expected exactly one snapshot delta"),
            "{e:?}"
        );
        assert_eq!(h.store().log_end(), end);
    });
}

#[test]
fn sdl5_empty_legacy_holder_refuses_feature_upload() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = rdy_store::format::legacy_writer_for_tests();
    let base = tempfile::tempdir().unwrap();
    let (_, mut fed, _) = project(&base.path().join("f"), 1);
    add_deltas(&mut fed);
    let ft = tag_hash(&fed, "v1.0.0");
    let file = base.path().join("fed.rdysync");
    export(&fed, ft, &file);
    let repo = fed.store().repo_id().unwrap().unwrap();
    upgrade::with_registry_for_tests(&steps(), || {
        let root = base.path().join("h");
        let mut h = holder(&root, repo, false);
        assert!(h.store().landings().unwrap().is_empty());
        let features = h.store().features().clone();
        let end = h.store().log_end();
        let e = publish(&mut h, &file, target(&fed, ft)).unwrap_err();
        assert_eq!(triple(&e), UPGRADE_REQUIRED);
        // the store's own take refuses it the same way, before anything is read further
        h.store_mut().set_held(false).unwrap();
        let taken = h.store_mut().take_batches(
            &[File::open(&file).unwrap()],
            &target(&fed, ft).upload,
            |_, _| {
                Ok::<_, rdy_store::stream_error::StreamError>(
                    rdy_store::take_batch::TagChoice::Incoming,
                )
            },
        );
        assert!(
            matches!(taken, Err(TakeError::Store(StoreError::UpgradeRequired))),
            "{taken:?}"
        );
        h.store_mut().set_held(true).unwrap();
        assert_eq!(h.store().log_end(), end);
        assert_eq!(h.store().features(), &features);
        h.close().unwrap();
        // once the host upgrades the holder, the same upload is taken
        let report = maintenance::run_with_registry_for_tests(&root, false, &steps()).unwrap();
        assert!(report.changed);
        let mut h = Rdy::open_holder(&root).unwrap();
        publish(&mut h, &file, target(&fed, ft)).unwrap();
        h.store().check_holder().unwrap();
    });
}
