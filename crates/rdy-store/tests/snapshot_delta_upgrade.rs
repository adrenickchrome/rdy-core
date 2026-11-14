//! sdl5 (D-918YB, D-8T7PA): the object-producing `snapshot_delta` upgrade step. Its one batch
//! holds one SNAPSHOT_DELTA for each held landing (off canon and holder forks too), then the
//! features ref and the receipt ref; a legacy SNAPSHOT that is not stored canonically refuses it
//! before anything is appended; every interruption keeps the acknowledged bytes and a restart
//! finishes the same upgrade with no second batch; the batch rule admits only the step's objects
//! and the two refs; an open of a store whose manifest is current walks no log; and a take of a
//! legacy upload into an upgraded holder converts it in its own batch and counts no SNAPSHOT
//! edge. The step is passed in as a test registry, and each test writes its legacy history as
//! the last rdy before snapshot deltas did (`format::legacy_writer_for_tests`).
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::format::{
    self, FEATURE_FIGURE_DARK, FEATURE_SNAPSHOT_DELTA, FEATURES_FILE, FIGURE_DARK_REQUIRED_RDY,
    FeatureClass, REF_STORE_FEATURES, REF_UPGRADE, SNAPSHOT_DELTA_REQUIRED_RDY, StoreFeature,
    StoreFeatures, UpgradeReceipt,
};
use rdy_store::holder::StoreMode;
use rdy_store::snapshot_upgrade::UPGRADE_BATCH;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::StreamHeader;
use rdy_store::take_batch::{
    HolderCapacity, TagChoice, TakeError, UploadFacts, UploadTarget, set_holder_capacity_for_tests,
};
use rdy_store::upgrade::{
    self, Phase, SNAPSHOT_DELTA_STEP, UpgradeReport, UpgradeStep,
    apply_locked_with_registry_for_tests as apply_with,
    inspect_with_registry_for_tests as inspect_with, with_hook_for_tests,
};
use rdy_store::{OpenPolicy, Store, StoreError, StoreLock, codec, kinds};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ---------------------------------------------------------------------------------------------
// fixtures

/// The registry these tests pass in: the snapshot-delta step alone.
fn steps() -> Vec<UpgradeStep> {
    vec![upgrade::snapshot_delta_step()]
}

fn sd_feature() -> StoreFeature {
    StoreFeature {
        name: FEATURE_SNAPSHOT_DELTA.into(),
        class: FeatureClass::Incompatible,
        required_rdy: SNAPSHOT_DELTA_REQUIRED_RDY.into(),
    }
}

fn figure_dark() -> StoreFeature {
    StoreFeature {
        name: FEATURE_FIGURE_DARK.into(),
        class: FeatureClass::Incompatible,
        required_rdy: FIGURE_DARK_REQUIRED_RDY.into(),
    }
}

fn prov(role: Role, at: i64) -> Provenance {
    Provenance {
        actor: "human:someone".into(),
        run: None,
        role,
        at_ms: at,
    }
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
        foreign: Default::default(),
        read_identity: None,
    }
}

/// Lands `ops` on canon through view `work`.
fn land(s: &mut Store, ops: Vec<Op>) -> Hash {
    let change = Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: prov(Role::Writer, 1),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    s.commit_to_view("work", change).unwrap();
    let p = s.propose("work", "next", prov(Role::Writer, 1)).unwrap();
    s.land(&p, prov(Role::Approver, 2)).unwrap()
}

/// A landing record like `of`'s, written again with `prev` and no changes: an off-canon landing
/// that keeps `state` as its state. `at` makes its hash its own.
fn forge(s: &mut Store, of: &Hash, prev: Option<Hash>, state: Hash, at: i64) -> Hash {
    let mut rec = s.landing(of).unwrap();
    rec.read_identity = None;
    rec.prev = prev;
    rec.changes = vec![];
    rec.state = state;
    rec.approved_by.at_ms = at;
    s.put(kinds::LANDING, &rec).unwrap()
}

/// A closed legacy project at `<base>/.rdy/store`: canon is a create, an update and a delete,
/// with tag `v1.0.0` on the last; with `forks`, a second root, a fork off the first landing and
/// a landing on that fork are held off canon.
struct Fx {
    dir: PathBuf,
    canon: Vec<Hash>,
    forks: Vec<Hash>,
}

fn project(base: &Path, forks: bool) -> Fx {
    let dir = base.join(".rdy").join("store");
    std::fs::create_dir_all(base.join(".rdy")).unwrap();
    let mut s = Store::create(&dir).unwrap();
    let b1 = block("b-00001", "one");
    let b2 = block("b-00002", "two");
    let l0 = land(
        &mut s,
        vec![
            Op::Create { block: b1.clone() },
            Op::Create { block: b2.clone() },
        ],
    );
    let l1 = land(
        &mut s,
        vec![
            Op::Update {
                base: b1.version_hash(),
                block: block("b-00001", "one, again"),
            },
            Op::Create {
                block: block("b-00003", "three"),
            },
        ],
    );
    let l2 = land(
        &mut s,
        vec![Op::Delete {
            base: b2.version_hash(),
            id: b2.id.clone(),
        }],
    );
    s.put_tag(&Tag {
        name: "v1.0.0".into(),
        landing: l2,
        message: Some("first".into()),
        tagged_by: prov(Role::Approver, 3),
    })
    .unwrap();
    let mut out = vec![];
    if forks {
        let s0 = s.landing(&l0).unwrap().state;
        let s1 = s.landing(&l1).unwrap().state;
        let root = forge(&mut s, &l0, None, s1, 100);
        let f1 = forge(&mut s, &l0, Some(l0), s0, 200);
        let f2 = forge(&mut s, &l1, Some(f1), s1, 300);
        out = vec![root, f1, f2];
    }
    s.check_objects().unwrap();
    s.close().unwrap();
    Fx {
        dir,
        canon: vec![l0, l1, l2],
        forks: out,
    }
}

/// Lists `extra` in the closed store's manifest, in its log and its mirror, as a store that
/// used it would.
fn add_features(dir: &Path, extra: &[StoreFeature]) -> StoreFeatures {
    let mut s = Store::open(dir).unwrap();
    let m = s.features().merged(extra);
    s.set_refs_unchecked_for_tests(vec![(REF_STORE_FEATURES.into(), m.to_json())])
        .unwrap();
    s.close().unwrap();
    format::write_format_file(dir, &m).unwrap();
    m
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

/// The SNAPSHOT_DELTA frames of the log, decoded, in log order.
fn delta_frames(dir: &Path) -> Vec<SnapshotDelta> {
    frames(dir)
        .into_iter()
        .filter(|(k, _, _)| codec::logical_kind(*k) == kinds::SNAPSHOT_DELTA)
        .map(|(k, h, p)| {
            let d: SnapshotDelta = codec::decode_canonical(k, &p).unwrap();
            assert_eq!(d.id(), h);
            d
        })
        .collect()
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
    std::fs::read_to_string(dir.join(FEATURES_FILE)).unwrap()
}

fn backup_name(from: &StoreFeatures, terminal: Hash) -> String {
    format!(".store-upgrade-{}-{terminal}", from.hash())
}

/// The upgrade backups beside the store at `dir`.
fn backups(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(upgrade::BACKUP_PREFIX))
        .collect();
    out.sort();
    out
}

/// Plans and applies the snapshot-delta upgrade on the closed store at `dir`.
fn upgrade(dir: &Path) -> Result<UpgradeReport, StoreError> {
    let lock = StoreLock::take(dir).unwrap();
    let plan = inspect_with(dir, &steps()).unwrap();
    apply_with(dir, &lock, &plan, &steps())
}

/// The engine's completion: the project's store check, or the holder's, on the upgrade's own
/// open, then the mirrors.
fn complete(dir: &Path) {
    let lock = StoreLock::take(dir).unwrap();
    let mut s = upgrade::open_pending_for_tests(dir, &lock).unwrap();
    if s.mode().unwrap() == StoreMode::Holder {
        s.check_holder().unwrap();
    } else {
        s.check_objects().unwrap();
    }
    upgrade::publish_checked(&mut s).unwrap();
    s.close().unwrap();
}

/// Each held landing's `prev`, `state` and full map.
type Held = BTreeMap<Hash, (Option<Hash>, Hash, BTreeMap<BlockId, Hash>)>;

/// Each held landing's `prev` and full map, read before the upgrade.
fn maps(dir: &Path) -> Held {
    let s = Store::open(dir).unwrap();
    let r = s.reader();
    let mut out = BTreeMap::new();
    for (h, rec) in r.project_landings().unwrap() {
        let m = s.get::<StateSnapshot>(&rec.state).unwrap().blocks;
        out.insert(h, (rec.prev, rec.state, m));
    }
    out
}

/// The deltas the step must write for `maps`, in its order: a landing after its `prev`, the
/// ready ones by hash, each against its `prev`'s map (the empty map at a root).
fn expected(maps: &Held) -> Vec<SnapshotDelta> {
    let mut left: BTreeMap<Hash, Option<Hash>> =
        maps.iter().map(|(h, (p, _, _))| (*h, *p)).collect();
    let mut made: BTreeMap<Hash, Hash> = BTreeMap::new();
    let mut out = vec![];
    while let Some(l) = left
        .iter()
        .filter(|(_, p)| p.is_none_or(|p| !left.contains_key(&p)))
        .map(|(h, _)| *h)
        .min()
    {
        let prev = left.remove(&l).unwrap();
        let empty = BTreeMap::new();
        let (base, before) = match prev {
            Some(p) => (Some(made[&p]), &maps[&p].2),
            None => (None, &empty),
        };
        let d = diff_snapshots(l, base, before, &maps[&l].2);
        assert_eq!(d.full, maps[&l].1, "the fixture's maps are canonical");
        made.insert(l, d.id());
        out.push(d);
    }
    assert!(left.is_empty(), "no prev cycle");
    out
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn sdl5_upgrade_writes_objects_features_and_receipt_once() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = format::legacy_writer_for_tests();
    let base = tempfile::tempdir().unwrap();
    let fx = project(base.path(), false);
    let dir = &fx.dir;
    // a store that uses figure_dark keeps it, with its version, through the step
    let from = add_features(dir, &[figure_dark()]);
    let to = from.merged(&[sd_feature()]);
    assert!(to.has(FEATURE_FIGURE_DARK) && to.has(FEATURE_SNAPSHOT_DELTA));
    let before = maps(dir);
    let (refs, tags, end) = {
        let s = Store::open(dir).unwrap();
        (
            s.refs_with_prefix("").unwrap(),
            s.tags().unwrap(),
            s.log_end(),
        )
    };
    let old_frames = frames(dir);
    let old_segments = segments(dir);
    let name = backup_name(&from, terminal(dir));

    // the dry run plans the one optional step, with the report shape it always had
    let plan = inspect_with(dir, &steps()).unwrap();
    assert_eq!(plan.steps, vec![SNAPSHOT_DELTA_STEP.to_string()]);
    assert_eq!(
        (plan.from.clone(), plan.to.clone()),
        (from.clone(), to.clone())
    );
    assert!(!plan.requires_upgrade, "readers do not require the step");
    assert!(!plan.pending_check);

    let report = upgrade(dir).unwrap();
    let receipt = UpgradeReceipt {
        from: from.hash(),
        to: to.hash(),
        steps: vec![SNAPSHOT_DELTA_STEP.into()],
        source_end: end,
        backup: name.clone(),
    };
    let new_frames = frames(dir);
    assert_eq!(
        report,
        UpgradeReport {
            dry_run: false,
            changed: true,
            from: from.clone(),
            to: to.clone(),
            steps: vec![SNAPSHOT_DELTA_STEP.into()],
            backup: Some(base.path().join(".rdy").join(&name)),
            batch: Some(new_frames.last().unwrap().1),
            reindexed: true,
            checked: false,
            receipt: Some(receipt.clone()),
            pending: false,
        }
    );
    // every old frame is where it was; one batch follows: the deltas, the features ref, the
    // receipt ref and the marker
    keeps_bytes(&old_segments, &segments(dir));
    assert_eq!(new_frames[..old_frames.len()], old_frames[..]);
    let added = &new_frames[old_frames.len()..];
    let kinds_added: Vec<u8> = added.iter().map(|(k, _, _)| *k).collect();
    assert_eq!(
        kinds_added,
        [
            kinds::SNAPSHOT_DELTA | 0x80,
            kinds::SNAPSHOT_DELTA | 0x80,
            kinds::SNAPSHOT_DELTA | 0x80,
            kinds::REF_UPDATE,
            kinds::REF_UPDATE,
            kinds::BATCH_COMMIT,
        ]
    );
    let refs_added: Vec<(String, Vec<u8>)> = added[3..5]
        .iter()
        .map(|(_, _, p)| {
            let u: kinds::RefUpdate = postcard::from_bytes(p).unwrap();
            (u.name, u.value)
        })
        .collect();
    assert_eq!(
        refs_added,
        vec![
            (REF_STORE_FEATURES.to_string(), to.to_json()),
            (REF_UPGRADE.to_string(), receipt.to_json()),
        ]
    );
    let want = expected(&before);
    assert_eq!(delta_frames(dir), want);
    assert_eq!(
        want.iter().map(|d| d.landing).collect::<Vec<_>>(),
        fx.canon,
        "canon, oldest first"
    );
    // the mirror stays old until the completion, which publishes the merged manifest
    assert_eq!(
        std::fs::read(dir.join(format::FORMAT_FILE)).unwrap(),
        b"rdy store format\nstore_format 19\n"
    );
    assert_eq!(format_text(dir), from.to_text());
    complete(dir);
    assert_eq!(format_text(dir), to.to_text());
    assert_eq!(
        std::fs::read(dir.join(format::FORMAT_FILE)).unwrap(),
        b"rdy store format\nstore_format 19\n"
    );

    // the reads are the same, every old ref and tag is kept, and each landing has its delta
    {
        let s = Store::open(dir).unwrap();
        assert_eq!(s.features(), &to);
        assert_eq!(s.tags().unwrap(), tags);
        for (n, v) in &refs {
            if n != REF_STORE_FEATURES {
                assert_eq!(s.get_ref(n).unwrap().as_ref(), Some(v), "{n}");
            }
        }
        let r = s.reader();
        for (l, (_, _, m)) in &before {
            assert_eq!(&r.snapshot_at(l).unwrap().blocks, m, "{l}");
            assert!(s.snapshot_delta_of(l).unwrap().is_some(), "{l}");
        }
        s.check_objects().unwrap();
    }

    // a second upgrade adds no object, ref, backup or marker
    let frames_done = frames(dir);
    let again = inspect_with(dir, &steps()).unwrap();
    assert!(again.steps.is_empty() && !again.pending_check, "{again:?}");
    let report = upgrade(dir).unwrap();
    assert!(!report.changed && report.batch.is_none() && report.backup.is_none());
    assert_eq!(frames(dir), frames_done);
    assert_eq!(backups(dir), vec![name]);
    assert_eq!(upgrade_batches(dir), 1);
}

#[test]
fn sdl5_upgrade_covers_off_canon_and_holder_forks() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = format::legacy_writer_for_tests();
    // a project: canon, a second root, a fork off canon's first landing and a landing on it
    let base = tempfile::tempdir().unwrap();
    let fx = project(base.path(), true);
    let before = maps(&fx.dir);
    assert_eq!(before.len(), 6);
    let old_frames = frames(&fx.dir);
    upgrade(&fx.dir).unwrap();
    let want = expected(&before);
    let got = delta_frames(&fx.dir);
    assert_eq!(
        got, want,
        "one delta per held landing, in prev order, ties by hash"
    );
    let held: BTreeSet<Hash> = got.iter().map(|d| d.landing).collect();
    assert!(fx.canon.iter().chain(&fx.forks).all(|l| held.contains(l)));
    // the fork's landing is based on the fork's delta, the second root on none
    let of = |l: &Hash| got.iter().find(|d| d.landing == *l).unwrap().clone();
    assert_eq!(of(&fx.forks[0]).base, None);
    assert_eq!(of(&fx.forks[1]).base, Some(of(&fx.canon[0]).id()));
    assert_eq!(of(&fx.forks[2]).base, Some(of(&fx.forks[1]).id()));
    assert!(
        of(&fx.forks[1]).set.is_empty() && of(&fx.forks[1]).drop.is_empty(),
        "an unchanged map"
    );
    assert_eq!(frames(&fx.dir)[..old_frames.len()], old_frames[..]);
    complete(&fx.dir);
    {
        let s = Store::open(&fx.dir).unwrap();
        let r = s.reader();
        for (l, (_, _, m)) in &before {
            assert_eq!(&r.snapshot_at(l).unwrap().blocks, m, "{l}");
        }
    }

    // a holder whose two tags name two forks of one history
    let hb = tempfile::tempdir().unwrap();
    let dir = hb.path().join("h");
    let mut h = Store::create_holder(&dir, RepoId([7; 16])).unwrap();
    let src = Store::open(&fx.dir).unwrap();
    let mut copy = |kind: u8, hash: &Hash| {
        let (raw, payload) = src.reader().get_raw(hash).unwrap();
        assert_eq!(codec::logical_kind(raw), kind);
        h.write(vec![(raw, payload)]).unwrap();
    };
    let mut landings: Vec<Hash> = vec![fx.canon[0], fx.canon[1], fx.forks[1], fx.forks[2]];
    for l in &landings {
        let rec = src.landing(l).unwrap();
        for c in &rec.changes {
            let change: Change = src.get(c).unwrap();
            for op in &change.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op {
                    copy(kinds::BLOCK, &block.version_hash());
                }
            }
            copy(kinds::CHANGE, c);
        }
        copy(kinds::PROPOSAL, &rec.proposal);
        copy(kinds::SNAPSHOT, &rec.state);
        copy(kinds::LANDING, l);
    }
    drop(copy);
    drop(src);
    for (name, l) in [("v1.0.0", fx.canon[1]), ("v2.0.0", fx.forks[2])] {
        let t = h
            .put(
                kinds::TAG,
                &Tag {
                    name: name.into(),
                    landing: l,
                    message: None,
                    tagged_by: prov(Role::Approver, 4),
                },
            )
            .unwrap();
        h.set_refs(vec![(format!("tag/{name}"), t.0.to_vec())])
            .unwrap();
    }
    h.check_holder().unwrap();
    let refs = h.refs_with_prefix("").unwrap();
    h.close().unwrap();
    let held = maps(&dir);
    landings.sort();
    assert_eq!(held.keys().copied().collect::<Vec<_>>(), landings);
    upgrade(&dir).unwrap();
    let want = expected(&held);
    assert_eq!(delta_frames(&dir), want);
    assert_eq!(want.len(), 4, "each fork's landings, once");
    complete(&dir);
    let h = Store::open(&dir).unwrap();
    assert_eq!(h.mode().unwrap(), StoreMode::Holder);
    assert!(h.features().has(FEATURE_SNAPSHOT_DELTA));
    for (n, v) in &refs {
        if n != REF_STORE_FEATURES {
            assert_eq!(h.get_ref(n).unwrap().as_ref(), Some(v), "{n}");
        }
    }
    let r = h.reader();
    for (l, (_, _, m)) in &held {
        assert_eq!(&r.snapshot_at(l).unwrap().blocks, m, "{l}");
    }
    h.check_holder().unwrap();
}

/// A SNAPSHOT of `map` (two entries or more) whose bytes decode but are not canonical: its
/// first two keys swapped.
fn reordered(map: &BTreeMap<BlockId, Hash>) -> Vec<u8> {
    assert!(map.len() >= 2);
    let mut entries: Vec<(&BlockId, &Hash)> = map.iter().collect();
    entries.swap(0, 1);
    let mut out = vec![entries.len() as u8];
    for (id, v) in entries {
        let s = id.to_string();
        out.push(s.len() as u8);
        out.extend(s.as_bytes());
        out.extend(v.0);
    }
    out
}

#[test]
fn sdl5_noncanonical_legacy_snapshot_leaves_store_readable() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = format::legacy_writer_for_tests();
    let base = tempfile::tempdir().unwrap();
    let fx = project(base.path(), false);
    let dir = &fx.dir;
    // a landing off canon whose SNAPSHOT holds its map in bytes that are not canonical
    let (bad, state) = {
        let mut s = Store::open(dir).unwrap();
        let m = s
            .get::<StateSnapshot>(&s.landing(&fx.canon[1]).unwrap().state)
            .unwrap()
            .blocks;
        let state = s.write(vec![(kinds::SNAPSHOT, reordered(&m))]).unwrap()[0];
        assert_ne!(state, StateSnapshot { blocks: m }.id());
        let l = forge(&mut s, &fx.canon[1], Some(fx.canon[2]), state, 500);
        s.close().unwrap();
        (l, state)
    };
    let old_segments = segments(dir);
    let old_features = format_text(dir);
    let old_format = std::fs::read(dir.join(format::FORMAT_FILE)).unwrap();
    let refs = Store::open(dir).unwrap().refs_with_prefix("").unwrap();
    let e = upgrade(dir).unwrap_err();
    let detail = format!("landing {bad}: snapshot {state} is not canonical");
    match &e {
        StoreError::Decode(m) => assert_eq!(m, &detail),
        other => panic!("{other:?}"),
    }
    // nothing is appended: the log, the refs and both mirrors are as they were
    assert_eq!(segments(dir), old_segments);
    assert_eq!(format_text(dir), old_features);
    assert_eq!(
        std::fs::read(dir.join(format::FORMAT_FILE)).unwrap(),
        old_format
    );
    assert_eq!(upgrade_batches(dir), 0);
    // a reader opens the legacy store as it is, with the step registered or passed in
    let read = |s: Store| {
        assert!(!s.features().has(FEATURE_SNAPSHOT_DELTA));
        assert_eq!(s.refs_with_prefix("").unwrap(), refs);
        assert_eq!(s.landings().unwrap().len(), 3);
        assert!(!s.upgrade_pending());
    };
    read(Store::open_with_registry_for_tests(dir, OpenPolicy::RequireCurrent, &steps()).unwrap());
    read(upgrade::with_registry_for_tests(&steps(), || {
        Store::open_with_policy(dir, OpenPolicy::RequireCurrent).unwrap()
    }));
    // the refused upgrade keeps its backup, by the existing rules; once a person moves it
    // aside, a writer's open with the step refuses the same way, and appends nothing either
    let backup = base.path().join(".rdy").join(backup_name(
        &Store::open(dir).unwrap().features().clone(),
        terminal(dir),
    ));
    assert!(backup.join("backup.json").exists());
    std::fs::rename(&backup, base.path().join("aside")).unwrap();
    let e = upgrade::with_registry_for_tests(&steps(), || Store::open(dir).err().unwrap());
    assert!(e.to_string().contains(&detail), "{e}");
    assert_eq!(segments(dir), old_segments);
    assert_eq!(format_text(dir), old_features);
}

/// A fault an upgrade meets.
#[derive(Clone, Copy, Debug)]
enum Fault {
    /// A backup file's copy.
    Copy,
    /// A backup segment's sync.
    Sync,
    /// The batch's first frame, torn.
    Append,
    /// The batch's third delta, torn after two whole ones.
    AppendMid,
    /// The marker's sync.
    Seal,
    /// The index update after the marker.
    IndexApply,
    /// The rebuild of the index.
    Reindex,
    /// The store check.
    Check,
}

impl Fault {
    fn sealed(self) -> bool {
        matches!(self, Fault::IndexApply | Fault::Reindex | Fault::Check)
    }
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
            (Fault::AppendMid, Phase::Opened(s)) => s.fail_append_at_frame_for_tests(Some((2, 9))),
            (Fault::Seal, Phase::Opened(s)) => s.fail_next_sync(),
            (Fault::IndexApply, Phase::Opened(s)) => s.fail_next_index_apply(),
            (Fault::Reindex, Phase::Sealed(s)) => s.fail_next_replay_after(0),
            (Fault::Check, Phase::Reindexed(_)) => {
                return Err(std::io::Error::other("injected check failure"));
            }
            _ => {}
        }
        Ok(())
    }
}

#[test]
fn sdl5_every_interruption_preserves_acknowledged_bytes() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = format::legacy_writer_for_tests();
    for fault in [
        Fault::Copy,
        Fault::Sync,
        Fault::Append,
        Fault::AppendMid,
        Fault::Seal,
        Fault::IndexApply,
        Fault::Reindex,
        Fault::Check,
    ] {
        let base = tempfile::tempdir().unwrap();
        let fx = project(base.path(), true);
        let dir = &fx.dir;
        let before = maps(dir);
        let old_segments = segments(dir);
        let old_frames = frames(dir);
        let old_format = format_text(dir);
        let plan = inspect_with(dir, &steps()).unwrap();
        let name = backup_name(&plan.from, terminal(dir));
        let backup = base.path().join(".rdy").join(&name);
        let e = with_hook_for_tests(inject(fault), || upgrade(dir)).unwrap_err();
        assert!(
            matches!(e, StoreError::Io(_) | StoreError::Db(_)),
            "{fault:?}: {e:?}"
        );
        keeps_bytes(&old_segments, &segments(dir));
        assert_eq!(frames(dir)[..old_frames.len()], old_frames[..], "{fault:?}");
        assert_eq!(format_text(dir), old_format, "{fault:?}: no mirror");
        assert!(backup.is_dir(), "{fault:?}: the backup is kept");
        if fault.sealed() {
            // the marker is durable: the receipt names the batch, the step is never run again,
            // the index is rebuilt from the log and the completion stays pending
            assert_eq!(upgrade_batches(dir), 1, "{fault:?}");
            assert_eq!(delta_frames(dir).len(), before.len(), "{fault:?}");
            let now = inspect_with(dir, &steps()).unwrap();
            assert!(now.pending_check && now.steps.is_empty(), "{fault:?}");
            assert_eq!(now.receipt.as_ref().unwrap().backup, name);
            let again = upgrade(dir).unwrap();
            assert_eq!(
                (again.changed, again.batch, again.backup),
                (false, None, None),
                "{fault:?}"
            );
            assert_eq!(upgrade_batches(dir), 1, "{fault:?}: no second batch");
        } else {
            // before the marker: exactly the acknowledged bytes, and the same plan
            assert_eq!(
                segments(dir),
                old_segments,
                "{fault:?}: nothing more is kept"
            );
            assert_eq!(upgrade_batches(dir), 0, "{fault:?}");
            assert_eq!(inspect_with(dir, &steps()).unwrap(), plan, "{fault:?}");
            if !backup.join("backup.json").exists() {
                // an incomplete backup is never used: a person moves it aside
                assert!(matches!(fault, Fault::Copy | Fault::Sync), "{fault:?}");
                assert!(upgrade(dir).is_err());
                assert_eq!(segments(dir), old_segments, "{fault:?}");
            }
            // a backup a failed upgrade left is the person's: it is moved aside, as before
            std::fs::rename(&backup, base.path().join("aside")).unwrap();
            let report = upgrade(dir).unwrap();
            assert!(report.changed, "{fault:?}");
            assert_eq!(upgrade_batches(dir), 1, "{fault:?}");
        }
        assert_eq!(delta_frames(dir), expected(&before), "{fault:?}");
        assert_eq!(frames(dir)[..old_frames.len()], old_frames[..], "{fault:?}");
        complete(dir);
        let s = Store::open(dir).unwrap();
        assert!(s.features().has(FEATURE_SNAPSHOT_DELTA), "{fault:?}");
        let r = s.reader();
        for (l, (_, _, m)) in &before {
            assert_eq!(&r.snapshot_at(l).unwrap().blocks, m, "{fault:?}: {l}");
        }
    }
}

/// The child of `sdl5_restart_finishes_without_a_second_upgrade_batch`: upgrades the store at
/// `RDY_SDL5_DIR`, prints `SDL5-PHASE <name>` at phase `RDY_SDL5_PHASE` (at the third copy for
/// `copy`) and waits there to be killed.
#[test]
#[ignore]
fn sdl5_upgrade_child() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = format::legacy_writer_for_tests();
    let Ok(dir) = std::env::var("RDY_SDL5_DIR") else {
        return;
    };
    let phase = std::env::var("RDY_SDL5_PHASE").unwrap();
    let mut copies = 0;
    let r = with_hook_for_tests(
        move |p: Phase<'_>| {
            let name = p.name();
            if name == "copy" {
                copies += 1;
            }
            if name == phase && (name != "copy" || copies == 3) {
                println!("SDL5-PHASE {name}");
                std::io::stdout().flush().unwrap();
                loop {
                    std::thread::park();
                }
            }
            Ok(())
        },
        || upgrade(Path::new(&dir)),
    );
    println!("SDL5-DONE {r:?}");
}

/// Runs the child on `dir` and kills it once it reports `phase`.
fn kill_at(dir: &Path, phase: &str) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "sdl5_upgrade_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("RDY_SDL5_DIR", dir)
        .env("RDY_SDL5_PHASE", phase)
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
        if line.contains(&format!("SDL5-PHASE {phase}")) {
            break;
        }
    }
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert_eq!(
        rdy_store::pio::ended(&status),
        rdy_store::pio::Ended::Killed,
        "{phase}: {status:?}"
    );
}

#[test]
fn sdl5_restart_finishes_without_a_second_upgrade_batch() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = format::legacy_writer_for_tests();
    for phase in [
        "copy",
        "backup_complete",
        "opened",
        "sealed",
        "reindexed",
        "checked",
    ] {
        let base = tempfile::tempdir().unwrap();
        let fx = project(base.path(), true);
        let dir = &fx.dir;
        let before = maps(dir);
        let old_segments = segments(dir);
        let old_frames = frames(dir);
        let old_format = format_text(dir);
        let plan = inspect_with(dir, &steps()).unwrap();
        let name = backup_name(&plan.from, terminal(dir));
        let backup = base.path().join(".rdy").join(&name);
        kill_at(dir, phase);
        keeps_bytes(&old_segments, &segments(dir));
        assert_eq!(
            format_text(dir),
            old_format,
            "{phase}: the mirror stays old"
        );
        let now = inspect_with(dir, &steps()).unwrap();
        if matches!(phase, "copy" | "backup_complete" | "opened") {
            // before the marker: nothing was appended, and the same step is planned
            assert_eq!(segments(dir), old_segments, "{phase}");
            assert!(
                now.steps == plan.steps && !now.pending_check,
                "{phase}: {now:?}"
            );
            let lock = StoreLock::take(dir).unwrap();
            let report = match apply_with(dir, &lock, &now, &steps()) {
                // a complete backup of the unchanged store is used as it is
                Ok(r) => {
                    assert_eq!(phase, "backup_complete");
                    r
                }
                Err(_) => {
                    assert_ne!(phase, "backup_complete");
                    std::fs::rename(&backup, base.path().join("aside")).unwrap();
                    apply_with(dir, &lock, &now, &steps()).unwrap()
                }
            };
            drop(lock);
            assert!(report.changed, "{phase}");
        } else {
            // after the marker: the receipt identifies the batch, which is never appended again
            assert_eq!(upgrade_batches(dir), 1, "{phase}");
            assert!(
                now.pending_check && now.steps.is_empty(),
                "{phase}: {now:?}"
            );
            let receipt = now.receipt.clone().unwrap();
            assert_eq!(receipt.steps, vec![SNAPSHOT_DELTA_STEP.to_string()]);
            assert_eq!(receipt.backup, name);
            let again = upgrade(dir).unwrap();
            assert!(!again.changed && again.batch.is_none(), "{phase}");
        }
        assert_eq!(upgrade_batches(dir), 1, "{phase}");
        assert_eq!(delta_frames(dir), expected(&before), "{phase}");
        assert_eq!(frames(dir)[..old_frames.len()], old_frames[..], "{phase}");
        assert_eq!(format_text(dir), old_format, "{phase}: still pending");
        // a completion that stops after its check and before the mirrors resumes from the
        // receipt: the same mirrors, in their order, with no second batch or backup
        {
            let lock = StoreLock::take(dir).unwrap();
            let s = upgrade::open_pending_for_tests(dir, &lock).unwrap();
            assert!(s.upgrade_pending(), "{phase}");
            s.check_objects().unwrap();
        }
        let pending = inspect_with(dir, &steps()).unwrap();
        assert!(pending.pending_check, "{phase}");
        let ((), synced) = rdy_store::fsutil::record_syncs(|| complete(dir));
        let at = |n: &str| synced.iter().position(|p| p == &dir.join(n)).unwrap();
        assert!(at(FEATURES_FILE) < at(format::FORMAT_FILE), "{phase}");
        let to = plan.to.clone();
        assert_eq!(format_text(dir), to.to_text(), "{phase}");
        complete(dir);
        let done = inspect_with(dir, &steps()).unwrap();
        assert!(!done.pending_check && done.steps.is_empty(), "{phase}");
        assert_eq!(upgrade_batches(dir), 1, "{phase}");
        assert_eq!(
            backups(dir),
            vec![name],
            "{phase}: one backup beside the store"
        );
    }
}

#[test]
fn sdl5_upgrade_batch_rejects_unregistered_objects_and_refs() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = format::legacy_writer_for_tests();
    let base = tempfile::tempdir().unwrap();
    let fx = project(base.path(), false);
    let dir = &fx.dir;
    let before = maps(dir);
    let want = expected(&before);
    let items = |ds: &[SnapshotDelta]| -> Vec<(u8, Vec<u8>)> {
        ds.iter()
            .map(|d| codec::item_new(kinds::SNAPSHOT_DELTA, d))
            .collect()
    };
    let mut s = Store::open(dir).unwrap();
    let from = s.features().clone();
    let to = from.merged(&[sd_feature()]);
    let end = s.log_end();
    let receipt = |steps: &[&str], from: Hash, to: Hash, end| {
        UpgradeReceipt {
            from,
            to,
            steps: steps.iter().map(|s| s.to_string()).collect(),
            source_end: end,
            backup: backup_name(&StoreFeatures::of(vec![]), Hash([1; 32])),
        }
        .to_json()
    };
    let good = receipt(&[SNAPSHOT_DELTA_STEP], from.hash(), to.hash(), end);
    let old_segments = segments(dir);
    let mut refuse = |features: &StoreFeatures, receipt: &[u8], objects, why: &str| {
        let e = s
            .commit_upgrade_for_tests(features, receipt, objects)
            .unwrap_err();
        assert!(e.to_string().contains(why), "{why}: {e}");
        assert_eq!(s.log_end(), end, "{why}: nothing appended");
    };
    let rec = |raw: u8, payload: Vec<u8>| vec![(raw, payload)];

    // objects without the step, and objects of any other kind, are not the step's
    let other = receipt(&["other"], from.hash(), to.hash(), end);
    refuse(&to, &other, items(&want), UPGRADE_BATCH);
    let mut with_block = items(&want);
    with_block.push(codec::item_new(kinds::BLOCK, &block("b-00009", "nine")));
    refuse(&to, &good, with_block, UPGRADE_BATCH);
    let mut with_ref = items(&want);
    with_ref.extend(rec(
        kinds::REF_UPDATE,
        postcard::to_allocvec(&kinds::RefUpdate {
            name: "tag/v9".into(),
            value: vec![1],
            at: end,
        })
        .unwrap(),
    ));
    refuse(&to, &good, with_ref, UPGRADE_BATCH);
    // a manifest that does not name the feature the deltas use
    let short = receipt(&[SNAPSHOT_DELTA_STEP], from.hash(), from.hash(), end);
    refuse(&from, &short, items(&want), UPGRADE_BATCH);
    // a missing delta, and a second one for the same landing
    let last = want[2].landing;
    let missing = format!("landing {last}: expected exactly one snapshot delta");
    refuse(&to, &good, items(&want[..2]), &missing);
    refuse(&to, &good, vec![], "expected exactly one snapshot delta");
    let mut twice = want.clone();
    let mut other_base = want[2].clone();
    other_base.base = Some(want[0].id());
    twice.push(other_base.clone());
    refuse(&to, &good, items(&twice), &missing);
    // a wrong full, a wrong base and a set that is not canonical
    let mut wrong_full = want.clone();
    wrong_full[2].full = want[1].full;
    refuse(
        &to,
        &good,
        items(&wrong_full),
        &format!(
            "snapshot delta {}: landing or full hash does not match",
            wrong_full[2].id()
        ),
    );
    let mut wrong_base = want.clone();
    wrong_base[2] = other_base;
    refuse(
        &to,
        &good,
        items(&wrong_base),
        &format!(
            "snapshot delta {}: base does not match the previous landing",
            wrong_base[2].id()
        ),
    );
    let mut unsorted = want.clone();
    assert!(unsorted[0].set.len() >= 2);
    unsorted[0].set.reverse();
    let id = codec::item_new(kinds::SNAPSHOT_DELTA, &unsorted[0]);
    refuse(
        &to,
        &good,
        items(&unsorted),
        &format!(
            "snapshot delta {}: set or drop is not canonical",
            Hash::of(&id.1)
        ),
    );
    // a receipt whose from, to or source end is not this store's
    for bad in [
        receipt(&[SNAPSHOT_DELTA_STEP], to.hash(), to.hash(), end),
        receipt(&[SNAPSHOT_DELTA_STEP], from.hash(), from.hash(), end),
        receipt(
            &[SNAPSHOT_DELTA_STEP],
            from.hash(),
            to.hash(),
            rdy_store::segment::Location {
                segment: end.segment,
                offset: end.offset - 1,
            },
        ),
    ] {
        let e = s
            .commit_upgrade_for_tests(&to, &bad, items(&want))
            .unwrap_err();
        assert_eq!(s.log_end(), end, "{e}");
    }
    assert_eq!(segments(dir), old_segments);
    // exactly the step's deltas and the two refs pass
    s.commit_upgrade_for_tests(&to, &good, items(&want))
        .unwrap();
    assert!(s.features().has(FEATURE_SNAPSHOT_DELTA));
    s.check_objects().unwrap();
    drop(s);
    assert_eq!(delta_frames(dir), want);
    assert_eq!(upgrade_batches(dir), 1);
}

#[test]
fn sdl5_current_manifest_open_does_not_walk_the_log() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = format::legacy_writer_for_tests();
    let base = tempfile::tempdir().unwrap();
    let fx = project(base.path(), false);
    let dir = &fx.dir;
    upgrade::with_registry_for_tests(&steps(), || {
        // a legacy store a reader opens as it is: the step is optional, the mirror says so, and
        // no log is walked
        let old_segments = segments(dir);
        let walks = upgrade::sealed_walks_for_tests();
        let s = Store::open_with_policy(dir, OpenPolicy::RequireCurrent).unwrap();
        assert!(!s.features().has(FEATURE_SNAPSHOT_DELTA) && !s.upgrade_pending());
        drop(s);
        assert_eq!(upgrade::sealed_walks_for_tests(), walks, "a reader's open");
        assert_eq!(segments(dir), old_segments);
        // a writer's open plans the step from the log and runs it
        let s = Store::open(dir).unwrap();
        assert!(s.features().has(FEATURE_SNAPSHOT_DELTA) && s.upgrade_pending());
        drop(s);
        assert!(upgrade::sealed_walks_for_tests() > walks);
        assert_eq!(upgrade_batches(dir), 1);
    });
    complete(dir);
    upgrade::with_registry_for_tests(&steps(), || {
        // the manifest the mirror holds is current: neither open walks the log
        let walks = upgrade::sealed_walks_for_tests();
        for policy in [OpenPolicy::AutoUpgrade, OpenPolicy::RequireCurrent] {
            let s = Store::open_with_policy(dir, policy).unwrap();
            assert!(s.features().has(FEATURE_SNAPSHOT_DELTA) && !s.upgrade_pending());
        }
        assert_eq!(
            upgrade::sealed_walks_for_tests(),
            walks,
            "a current store's opens"
        );
    });
    assert_eq!(upgrade_batches(dir), 1);
}

// ---------------------------------------------------------------------------------------------
// the take of a legacy upload into an upgraded holder

/// A legacy source project with `n` landings on canon and tag `v1` on the last.
struct Src {
    _dir: tempfile::TempDir,
    path: PathBuf,
    repo: RepoId,
    tag: Hash,
    landing: Hash,
}

fn source(n: usize) -> Src {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    let repo = s.repo_id().unwrap().unwrap();
    let mut last = None;
    for i in 0..n {
        let mut ops = vec![Op::Create {
            block: block(&format!("b-{:05}", i + 1), "x"),
        }];
        ops.push(Op::Create {
            block: block(&format!("c-{:05}", i + 1), "y"),
        });
        last = Some(land(&mut s, ops));
    }
    let landing = last.unwrap();
    let tag = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing,
            message: None,
            tagged_by: prov(Role::Approver, 3),
        })
        .unwrap();
    s.close().unwrap();
    Src {
        _dir: dir,
        path,
        repo,
        tag,
        landing,
    }
}

/// The whole history of tag `v1` as one batch file at `to`.
fn legacy_batch(src: &Src, to: &Path) {
    let s = Store::open(&src.path).unwrap();
    let r = s.reader();
    let c = closure(&r, src.tag, &BTreeSet::new()).unwrap();
    let header = StreamHeader::tag_history(
        r.store_features().unwrap(),
        src.repo,
        src.tag,
        c.prerequisites,
    );
    assert!(!header.features.has(FEATURE_SNAPSHOT_DELTA));
    let mut source = StoreSource {
        reader: &r,
        end: END_OF_LOG,
    };
    write_batch(&mut source, &header, &c.records, File::create(to).unwrap()).unwrap();
}

/// An empty holder of `repo` at `dir`, upgraded with the step and completed: it holds the feature
/// with no delta.
fn upgraded_holder(dir: &Path, repo: RepoId) {
    Store::create_holder(dir, repo).unwrap().close().unwrap();
    upgrade(dir).unwrap();
    complete(dir);
    let h = Store::open(dir).unwrap();
    assert!(h.features().has(FEATURE_SNAPSHOT_DELTA));
}

fn incoming(_: &mut dyn ObjectSource, _: &UploadFacts) -> Result<TagChoice, StreamError> {
    Ok(TagChoice::Incoming)
}

fn take_into(dir: &Path, batch: &Path, target: &UploadTarget) -> Result<(), String> {
    let mut h = Store::open(dir).unwrap();
    let f = File::open(batch).unwrap();
    match h.take_batches(std::slice::from_ref(&f), target, incoming) {
        Ok(_) => Ok(()),
        Err(TakeError::Stream(e)) => Err(format!("{}: {}", e.code, e.message)),
        Err(other) => Err(format!("{other:?}")),
    }
}

#[test]
fn sdl5_legacy_take_into_upgraded_holder_counts_no_snapshot_edges() {
    // a legacy history, written as the last rdy before snapshot deltas wrote it
    let _legacy = format::legacy_writer_for_tests();
    let src = source(3);
    let base = tempfile::tempdir().unwrap();
    let batch = base.path().join("legacy.rdysync");
    legacy_batch(&src, &batch);
    let target = UploadTarget {
        tag: "v1".into(),
        landing: src.landing,
    };
    let holders: Vec<PathBuf> = (0..3).map(|i| base.path().join(format!("h{i}"))).collect();
    for h in &holders {
        upgraded_holder(h, src.repo);
    }
    // a twin takes the upload with no lowered bound: the take converts each new landing
    let (objects, edges, raw_edges) = {
        take_into(&holders[0], &batch, &target).unwrap();
        let h = Store::open(&holders[0]).unwrap();
        assert!(h.features().has(FEATURE_SNAPSHOT_DELTA));
        let r = h.reader();
        let mut snaps = 0;
        r.for_each_object_of(kinds::SNAPSHOT, |_| {
            snaps += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            snaps, 3,
            "the upload's SNAPSHOT records are written as they came"
        );
        let landings = r.project_landings().unwrap();
        assert_eq!(landings.len(), 3);
        for (l, rec) in &landings {
            let (d, _) = h.snapshot_delta_of(l).unwrap().expect("a delta");
            let (raw, payload) = r.get_raw(&d).unwrap();
            let delta: SnapshotDelta = codec::decode_record(raw, &payload).unwrap();
            assert_eq!(delta.full, rec.state);
            assert_eq!(
                r.snapshot_at(l).unwrap().id(),
                rec.state,
                "the fold is the landing's map"
            );
        }
        let (objects, edges) = r.holder_counts().unwrap();
        let table = h.index_dump_for_tests().unwrap().holder_counts;
        let count = |k: &[u8]| u64::from_le_bytes(table[k].clone().try_into().unwrap());
        assert_eq!(count(b"snapshot_objects"), 3);
        assert_eq!(count(b"state_edges"), 3);
        assert_eq!(count(b"objects") - objects, 3, "no SNAPSHOT counts");
        // the legacy SNAPSHOT edges are the difference: they alone exceed the bound below
        let raw_edges = count(b"edges");
        assert!(raw_edges > edges, "{raw_edges} > {edges}");
        h.check_holder().unwrap();
        // equal to a fresh reindex
        drop(r);
        let mut h = h;
        let before = h.index_dump_for_tests().unwrap();
        h.reindex().unwrap();
        let after = h.index_dump_for_tests().unwrap();
        assert_eq!(before.holder_counts, after.holder_counts);
        assert_eq!(before.landing_depths, after.landing_depths);
        assert_eq!(h.reader().holder_counts().unwrap(), (objects, edges));
        h.reader()
            .check_holder_bounded_for_tests(3, objects as usize, edges as usize)
            .unwrap();
        (objects, edges, raw_edges)
    };
    // at the converted counts the take fits, though the legacy edges do not; one edge below
    // it is refused before anything is written
    let bound = |edges: u64| {
        set_holder_capacity_for_tests(Some(HolderCapacity {
            landing_depth: 3,
            objects,
            edges,
        }))
    };
    assert!(raw_edges > edges);
    bound(edges);
    let ok = take_into(&holders[1], &batch, &target);
    bound(edges - 1);
    let end = Store::open(&holders[2]).unwrap().log_end();
    let refused = take_into(&holders[2], &batch, &target);
    set_holder_capacity_for_tests(None);
    ok.unwrap();
    let e = refused.unwrap_err();
    assert!(e.starts_with("E_LIMIT"), "{e}");
    assert!(e.ends_with(&format!("{} edges", edges - 1)), "{e}");
    assert_eq!(Store::open(&holders[2]).unwrap().log_end(), end);
    let h = Store::open(&holders[1]).unwrap();
    assert_eq!(h.reader().holder_counts().unwrap(), (objects, edges));
    h.reader()
        .check_holder_bounded_for_tests(3, objects as usize, edges as usize)
        .unwrap();
    h.check_holder().unwrap();
}
